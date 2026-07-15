# HDR implementation — final review and release-readiness audit

**Date:** 2026-07-14
**Scope:** the complete HDR path from media metadata to presented pixels, plus SDR/audio regression, tests, docs, and runtime validation.
**Repository state audited:** `main` @ `2b7d724` ("Point the README download link at v0.4.2"), in sync with `origin/main`.

---

## 1. Executive summary

The shipped HDR implementation is **correct, internally consistent, honestly documented, and conservatively gated**. HDR10 (PQ) and HLG content is tone-mapped to SDR in FastPlay's own pixel shader and presented through the unchanged, pixel-verified SDR swapchain. Native HDR passthrough is deliberately **not** implemented; the passthrough skeleton in the tree is production-unreachable and clearly marked.

This audit independently reproduced the tone-map shader's pixel-exactness (GPU output ≤1/255 vs a double-precision CPU model of the shader, on PQ midtones, PQ clipped bars, and HLG bars), re-ran every committed verifier (all pass, including the negative control), exercised the classifier edge cases at runtime (contradictory/ambiguous HDR is declined with a typed error; SDR and audio-only are unaffected), and confirmed builds/tests/lints are clean.

**Verdict: Safe to merge and release.** All findings are Low/Informational except two Medium validation-hygiene items (an uncommitted golden harness backing a release-notes claim, and stale scope documents that now contradict the shipped feature). Nothing blocks merge or release; recommended fixes are listed in §13–§15.

---

## 2. Repository state

- Root: `C:\Users\Calvin\Software Projects\FastPlay`; single Rust crate.
- Branch `main` @ `2b7d724424522de83a143fbecad14c9961775fc2`, tracking `origin/main`, no divergence.
- Working tree: modified `assets/icon/fastplay.ico`, untracked `assets/icon/fastplay.png` — icon work, unrelated to HDR, left untouched.
- No uncommitted HDR changes. `validation/` holds historical committed validation logs (pre-HDR milestones); `bench/results/` holds June benchmark outputs. No stray HDR scratch artifacts found in the tree.
- Other local branches (`media-compat-audio-only-hdr-gating` = same commit as main; `phase3-persistent-scrub`, viewport refactor branches, one agent worktree) — none relevant to the audited state.

## 3. Chronological HDR history (from git, oldest first)

1. `8ab179b` — fixed washed-out SDR by tagging explicit YCbCr color spaces on `VideoProcessorBlt`. Foundation: made SDR colorimetry explicit and pixel-verifiable.
2. `4f44a9f` — HDR presentation-path skeleton: pure classifier (`classify_color_tags`), path decision (`select_video_presentation_path`), capability model, typed `HdrError`s. SDR preserved byte-for-byte (regression-tested constructor identity).
3. `a9df909` — GPU backbuffer readback harness, calibrated on SDR (`bench/verify-colors.ps1`).
4. `a400533` — HDR10 DXGI color spaces resolved and validated structurally (`CheckColorSpaceSupport`, `CheckVideoProcessorFormatConversion`) and by pixel (`bench/verify-colors-pq.ps1`, with a wrong-matrix negative control); env-gated `hdr_validate` entry added.
5. `5082abe` — retired HDR-VERIFY markers made stale by that resolution.
6. `5c3cede`, `beec0d1` — classifier policy made explicit and tested: SDR-default for untagged content, contradictory/incomplete PQ and HLG dead-end as `Unknown`, never silently SDR.
7. `e0dc704` — audio-only playback added; **first HDR play attempt** via D3D11 VideoProcessor HDR→SDR conversion, gated on `CheckVideoProcessorFormatConversion`. On the dev GPU the gate never passed, so HDR .mov "failed gracefully."
8. `ca14ff6` — **abandoned the VideoProcessor approach** with recorded evidence: on an RTX 3080 Ti the processor accepts no GHLG input in any combination, and PQ only to linear-scRGB or HDR10 outputs — never to 8-bit sRGB. Replaced with FastPlay's own tone-map pixel shader (`HdrToneMapRenderer`); PQ and HLG now play.
9. `b603f6f` — audio kept alive when a seek cancels the audio worker's open (tri-state `AudioOpen`).
10. `43c33c7` — `bench/verify-subtitles-hdr.ps1`: overlays composite correctly on the HDR shader path (which clears/binds the render target itself).
11. `84ad649` — FFmpeg's D3D11VA device ctx given its own device reference (AddRef); fixed the access-violation crash on rapid file switching, including HDR queues.
12. `ce94474`, `2b7d724` — v0.4.2 release notes, version bump, README.

The final architecture does **not** match the original plan (VideoProcessor conversion); the pivot is documented in code, commit history, and release notes.

## 4. Final architecture map

```
container/stream tags (AVCodecParameters primaries+trc, matrix, range)
  → classify_stream_color → classify_color_tags → ContentColorMode
      Sdr ──────────────────────────────→ ExistingSdr (zero new COM work at open)
      Hdr10Pq / Hlg / Unknown
        → query_hdr_presentation_capabilities(device, None)   [worker has no window;
            display flags conservatively false; display_hdr_active is ALWAYS false]
        → select_video_presentation_path
            Hdr10Pq + full caps → Hdr10Passthrough   [UNREACHABLE in production]
            Hdr10Pq otherwise  → HdrToSdrToneMapRequired
            Hlg                → HdrToSdrToneMapRequired
            Unknown            → UnsupportedHdr → typed OpenFailed

HdrToSdrToneMapRequired (the only live HDR path):
  tone_map_stream_color_space  [rejects full-range PQ, constant-luminance and
                                non-BT.2020 matrices as typed errors]
  → supports_hdr_shader_tone_map  [probes NV12+P010 plane SRVs at open;
                                   incapable device → clean OpenFailed]
  → DecodeSession.tone_map_input = resolved DXGI color space
  → every VideoSurface stamped hdr_tone_map = Some(space)
      HW decode: D3D11VA NV12/P010 → copy into BIND_DECODER|SHADER_RESOURCE texture
      SW decode: sws → 8-bit NV12 → upload (same bind flags); tag preserved
      HW→SW mid-file fallback preserves tone_map_input

render (DxgiSwapChain::render_video, per surface):
  hdr_tone_map == None → SDR VideoProcessor blt   [hard-errors on HDR surfaces]
  hdr_tone_map == Some → HdrToneMapRenderer (lazily built)
      shader: BT.2020 NCL YCbCr→R'G'B' → saturate
              → PQ EOTF ×(10000/203)  |  HLG invOETF → OOTF γ=1.2 ×(1000/203)
              → per-channel knee(0.75)/exponential shoulder → asymptote 1.0
              → BT.2020→BT.709 (linear) → saturate → sRGB encode
      [SDR path hard-errors if handed a tone-map surface — cross-routing impossible]

swapchain: ALWAYS DXGI_FORMAT_B8G8R8A8_UNORM flip-discard; production never calls
SetColorSpace1 or SetHDRMetaData; Present(1, 0). No HDR state exists to leak
across files, seeks, resizes, or device recovery (recovery re-runs the full open
gate on the new device).
```

## 5. Supported HDR matrix (verified on this machine unless noted)

| Content | Behavior | Verified |
|---|---|---|
| PQ + BT.2020 primaries, limited range, NCL/unspecified matrix (H.264 8-bit, HEVC 10-bit `hvc1` .mov/.mp4) | Plays, tone-mapped to SDR; seeks | Runtime + golden pixel |
| HLG + BT.2020 (8-bit H.264; 10-bit HEVC .mov, "iPhone-shaped") | Plays, tone-mapped; seeks | Runtime + golden pixel |
| HLG full-range (JPEG) | Maps to `YCBCR_FULL_GHLG_TOPLEFT_P2020`, full-range levels | Unit tests only (no pixel test; rare content) |
| PQ full-range | Declined at open (no DXGI space exists) | Unit tests |
| PQ + non-BT.2020 or unspecified primaries | `Unknown` → typed OpenFailed | Runtime |
| HLG + non-BT.2020 or unspecified primaries | `Unknown` → typed OpenFailed | Unit tests (same code path as PQ case, runtime-verified for PQ) |
| BT.2020 primaries + unspecified transfer | `Unknown` → typed OpenFailed | Runtime |
| PQ with real mastering-display/CLL SEI | Plays; metadata ignored (documented) | Runtime |
| HDR10 passthrough on HDR display | **Not implemented**; tone-mapped SDR shown instead | By construction (`display_hdr_active` always false) |
| Untagged-but-actually-HDR | Plays washed out through SDR path (documented policy) | By policy/code |
| HDR via software decode (`--force-sw`) | Plays, seeks; 10-bit reduced to 8-bit NV12 (banding possible) | Runtime |

SDR wide-gamut (BT.2020 primaries + SDR transfer) classifies SDR — plays through the SDR path (its wide gamut is not converted; pre-existing SDR behavior).

## 6. SDR / audio matrix (this audit's regression checks)

- SDR BT.709 tagged and fully untagged H.264: play, seek; backbuffer within ±2/255 of ffmpeg reference (`verify-colors.ps1` PASS).
- Audio-only `.mp3`: opens as audio-only (`[no_video_stream]`), first audio in 32 ms, graceful close. (The 7-format audio matrix and audio-only seek stability were validated at `e0dc704`/`b603f6f`; this audit spot-checked mp3 and relies on those commits' committed tests for the rest.)
- Rapid sequential opens across all 8 generated classes: every process exited 0 with clean logs.
- Subtitle compositing identical on HDR and SDR paths (13323 band pixels changed, 0 above the band, both paths).

## 7. Color-transform table (tone-map path, the only live HDR path)

| Stage | Value |
|---|---|
| Input | NV12 (8-bit) or P010 (10-bit) YCbCr, BT.2020 NCL, studio range (PQ, HLG) or full range (HLG only) |
| Level normalization | CPU-computed `ToneMapParams` (unit-tested: 64/940 ↔ 0/1 at 10-bit, 16/235 at 8-bit, P010 high-bit rescale 65535/65472) |
| Matrix | BT.2020 NCL, coefficients verified against Kr=0.2627/Kb=0.0593 derivation |
| Transfer | PQ: exact ST 2084 EOTF constants; HLG: exact BT.2100 inverse OETF + OOTF (γ=1.2, per-luminance so hue is preserved) |
| Normalization | diffuse white = 203 cd/m² (BT.2408); PQ absolute ×10000/203, HLG nominal peak ×1000/203 |
| Tone curve | identity ≤0.75, C1-continuous exponential shoulder, asymptote 1.0; per-channel (deliberate film-like highlight desaturation, documented) |
| Gamut | BT.2020→BT.709 in linear light (standard matrix), out-of-gamut clipped by saturate |
| Output | sRGB-encoded 8-bit B8G8R8A8, full range |

None of the failure classes searched for were found: no PQ-as-linear, no HLG/PQ swap (unit-tested selector + runtime golden test per transfer), no double EOTF, no missing gamut conversion, no range errors (golden tests pin black/white), no raised blacks/crushed shadows in measurements. `saturate` before the EOTF clips illegal super-black/super-white excursions — standard, documented behavior. Chroma siting is ignored (≤½ chroma texel shift; documented; flat-region color unaffected).

## 8. Hardware and display capability behavior

- The open-time gate is `supports_hdr_shader_tone_map`: probes creation of NV12 **and** P010 plane SRVs on throwaway textures. Requiring both is documented (bit depth unknown at open; mid-file HW→SW fallback can switch formats). An incapable device declines the file at open with a typed, user-readable error — never a first-draw failure (which device recovery would misread as device-lost).
- Production performs **no** display queries: the decode worker calls `query_hdr_presentation_capabilities(device, None)`, so all display-dependent capabilities stay false and PQ/HLG always routes to tone-map. Output is plain SDR; DWM handles HDR-active desktops as it does any SDR window (SDR-white-level slider applies). Monitor moves/HDR toggles cannot change FastPlay's behavior mid-session by construction. (Behavior on an HDR-active desktop was not visually assessed on this machine — see §12.)
- The code carefully distinguishes decode capability, VP input support, VP conversion support, swapchain color-space support, display capability vs activity — each its own field, each conservatively false until verified, with `CheckVideoProcessorFormatConversion` checked on the exact (format, space) → (format, space) tuple in the dev harness. No overbroad "HDR supported" boolean exists.

## 9. Test and build results (all run 2026-07-14 on this machine)

| Command | Exit | Result |
|---|---|---|
| `cargo fmt --check` | 0 | clean |
| `cargo build` (debug) | 0 | 0 warnings |
| `cargo clean` + `cargo build --release` | 0 | clean from scratch |
| `cargo test` | 0 | **187 passed, 0 failed** (matches release-notes claim; deterministic) |
| `cargo clippy --all-targets` | 0 | 2 pre-existing `large_enum_variant` warnings (`VideoOpen`, `AudioOpen`) — not HDR-related |
| `bench/verify-colors.ps1` (SDR pixel) | 0 | PASS, max delta 2/255 (tol 4) |
| `bench/verify-colors-pq.ps1` (HDR10 skeleton pixel) | 0 | PASS, max delta 8/1023 (tol 12); `CheckColorSpaceSupport` + conversion check accepted |
| `bench/verify-colors-pq.ps1 -WrongMatrix` (negative control) | 0 | control OK: delta 42 ≫ 12; conversion check correctly rejects wrong space |
| `bench/verify-subtitles-hdr.ps1` | 0 | PASS on HLG and SDR control |
| Scratch tone-map golden check (see §10) | 0 | PASS: PQ bars 0/255, PQ midtones ≤1/255, HLG ≤1/255 vs CPU model |
| Runtime classification matrix (8 cases) | 0 | all behave as designed (§5, §6) |
| `--force-sw` HLG 10-bit | 0 | plays + seeks, `decode_mode=SW` |

All results reproduced at least once within the session where re-run; unit tests are pure logic and fully deterministic.

## 10. Pixel/visual validation status

- **SDR identity**: committed harness, PASS (±2/255).
- **HDR10 skeleton color spaces** (matrix + range, PQ-encoded both sides, no linearization): committed harness with negative control, PASS.
- **Tone-map output (PQ→SDR, HLG→SDR, EOTF/OOTF/tone-curve/gamut/encode)**: `ca14ff6` and the v0.4.2 release notes claim pixel verification against a CPU model of the shader, but **that harness was never committed**. This audit reconstructed it (synthetic PQ/HLG clips → real player → app screenshot path → double-precision CPU model of the shader fed the exact decoded NV12 bytes): GPU matches the model at ≤1/255 across PQ clipped bars, PQ unclipped midtones, and HLG bars. The claim is therefore **true and now independently reproduced**, but the repo itself still cannot prove it — see finding F1.
- Not pixel-validated: full-range HLG (unit-tested at parameter level only), P010 input at pixel level (played correctly at runtime; level math unit-tested), chroma siting, behavior on an HDR-active desktop.

## 11. Findings (ordered by severity)

### Confirmed defects

- **F1 — Medium (validation hygiene).** The tone-map golden-pixel harness backing the release-notes claim "verified by pixel against a CPU model of the shader" was never committed; only the skeleton (`verify-colors-pq.ps1`) and subtitle verifiers are in `bench/`. Evidence: `git show ca14ff6 --stat` (src-only), `bench/` contents. Impact: color-accuracy claims were unreproducible from the repo until reconstructed by this audit. Fix: commit a `bench/verify-tonemap.ps1` (the audit's scratch script is a working starting point). Blocks merge: no. Blocks release: no. Confidence: high.
- **F2 — Medium (docs contradict shipped behavior).** `AGENTS.md` ("Do not add … HDR tone mapping"), `ARCHITECTURE.md` §2/§21 (HDR tone mapping deferred), `docs/ROADMAP.md` §5 and `docs/TECH_DEBT.md` §5 (explicit non-goals) all still forbid/defer HDR tone mapping, which shipped in v0.4.2. In an agent-driven repo these instruction files are treated as binding; a future task could wrongly refuse or unwind HDR work. Fix: one-line updates acknowledging the shipped tone-map path and scoping what remains out (passthrough, metadata-driven tone mapping). Blocks: no. Confidence: high.
- **F3 — Low (stale design comments).** Three doc comments still describe the abandoned VideoProcessor tone-map design: `src/render/swapchain.rs:33-38` and `:58-65` ("tone-mapping is performed by the video processor at blt time… the driver tone-maps"), `src/ffi/d3d11.rs:230-236` (`VideoSurface::hdr_tone_map`: "tone-mapped … by the video processor … driver performs HDR→SDR conversion"). The actual path is the pixel shader (`ca14ff6`). Misleads maintainers; no runtime impact. Blocks: no. Confidence: high.
- **F4 — Low (inaccurate perf comment + minor churn).** `src/ffi/d3d11.rs:130-132` claims "the whole path allocates nothing per frame," but `render_video_surface_tone_mapped` creates two `ID3D11ShaderResourceView`s per frame (`d3d11.rs:1315-1316`). View creation is cheap but is a per-frame kernel-mode allocation. Fix comment; optionally cache views per texture. Blocks: no. Confidence: high.
- **F5 — Low (hidden dead code / latent tripwire).** `src/ffi/ffmpeg.rs` has module-wide `#![allow(dead_code)]` (needed for bindgen), which silently hides that `refine_color_from_first_frame` and `extract_hdr_metadata_from_frame` are never called. Additionally, `extract_hdr_metadata_from_frame` **errors when metadata is present**; if the future passthrough commit wires it as its integration comment describes, every real HDR10 file carrying mastering SEI would fail to open. Runtime-verified today: a PQ file with real mastering/CLL SEI plays fine (function never runs). Fix: explicit `#[allow(dead_code)]` on the skeleton items + a comment on the metadata-presence error. Blocks: no. Confidence: high.
- **F6 — Low (diagnostics).** Nothing logs the chosen classification, presentation path, resolved color space, or tone-map gate outcome — only failures surface. A field report of "HDR looks wrong" is undebuggable from `session.log`. Fix: one `flog!` line at open (mode, path, resolved DXGI space, texture format). Blocks: no. Confidence: high.

### Reproducible limitations (documented, by design)

- HDR is always tone-mapped to SDR; no passthrough; mastering/CLL metadata ignored; fixed 203-nit diffuse-white target (README + release notes state all of this).
- PQ full-range YCbCr declined (no DXGI color space exists for it).
- Contradictory/incomplete HDR signalling declined with a typed error (user-facing message names the "HDR-tagged combination" cause).
- Untagged-but-actually-HDR content plays washed out through the SDR path (explicit, commented policy; fixing requires frame-level refinement work).
- Software-decoded 10-bit HDR is reduced to 8-bit NV12 before tone mapping (possible banding; colorimetrically correct; not user-documented — worth a line in Known limitations).

### Unverified risks

- **U1.** The `Hdr10Passthrough` arm at `ffmpeg.rs:360-368` returns an open **failure** if ever selected. It is unreachable today (`display_hdr_active` hardcoded false at `dxgi.rs:1240-1241`), but wiring display-activity detection without implementing passthrough would regress HDR-on-HDR-display from "plays tone-mapped" to "fails to open." Guarded only by HDR-VERIFY comments.
- **U2.** In-process mixed-queue HDR↔SDR↔failed-HDR switching could not be independently reproduced (queues require OLE drag-drop; the CLI seeds a single-file queue; synthesized `WM_DROPFILES` is ignored since the app uses `IDropTarget`). The release notes' 150-press validation stands unreplicated; per-class opens, seeks, and the shared-device fix (`84ad649`) were verified by other means. Suggest a folder-argument CLI affordance to make this testable.
- **U3.** Behavior on other GPU vendors: the tone-map path uses only core D3D11 (plane SRVs, ps_4_0), probed at open with a clean decline — low risk, but only NVIDIA was exercised.
- **U4.** Appearance of the tone-mapped SDR output on an HDR-active Windows desktop (DWM SDR-white-level composition) was not visually assessed.

### Future improvements (non-blocking)

- HDR10 passthrough (the skeleton's purpose); display-activity detection; metadata-aware tone mapping; full-range-HLG and P010 pixel-level goldens; chroma-siting-aware sampling; per-texture SRV caching.

## 12. Missing validation (summary)

Committed golden coverage for the tone-map transform itself (F1); full-range HLG pixel test; P010 pixel test; in-process queue traversal (U2); HDR-desktop visual check (U4); non-NVIDIA hardware (U3).

## 13. Required pre-merge fixes

None. The tree builds clean, tests pass, and no Critical/High defects exist.

## 14. Required pre-release fixes

None strictly required (v0.4.2 is already tagged and its claims are accurate — and the one previously-unreproducible claim has now been reproduced). Strongly recommended before the next release:

1. Commit the tone-map golden verifier as `bench/verify-tonemap.ps1` (F1).
2. Update `AGENTS.md` / `ARCHITECTURE.md` §21 / `ROADMAP` / `TECH_DEBT` scope lists to reflect shipped HDR tone mapping (F2).

## 15. Non-blocking follow-up plan (priority order)

1. F2 doc-scope updates (5 minutes, prevents agent/contributor confusion).
2. F1 commit the golden harness.
3. F3/F4/F5 comment corrections + explicit dead-code annotations.
4. F6 one-line open diagnostics.
5. U1: make the passthrough arm fall back to tone-map (or keep the error but add a test pinning that `display_hdr_active` stays false until passthrough exists).
6. U2: folder CLI argument (also unlocks scripted queue testing).
7. Software-decode 10-bit banding note in Known limitations.

## 16. Final verdict

**Safe to merge and release.** The HDR implementation is colorimetrically defensible (exact standard constants, verified by derivation and by reproduced pixel measurement), fails closed on everything it cannot present correctly, leaves the pixel-verified SDR and audio paths untouched (verified by measurement, not just by claim), and its documentation matches its actual behavior. The remaining work is hygiene and future passthrough, not correctness.
