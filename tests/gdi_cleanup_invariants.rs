//! Source invariants for the GDI cleanup seam in `src/ffi/d3d11.rs`.
//!
//! Release builds once leaked every font, bitmap, and DC the overlay
//! rasterizers created: all 36 cleanup sites were written
//! `debug_assert!(DeleteObject(..).as_bool())`, and `debug_assert!` does not
//! merely skip the *check* when `debug_assertions` is off — it does not
//! evaluate the expression at all. Roughly 8.5 handles leaked per timeline
//! overlay rebuild, against a 10,000-handle per-process quota and a 65,536
//! session-wide ceiling; a dozen instances exhausted the shared pool and took
//! unrelated processes down with them.
//!
//! The fix routed every delete through two helpers. These tests keep it that
//! way: raw `DeleteObject`/`DeleteDC` calls may exist *only* inside those
//! helpers, and the helpers must perform the delete unconditionally.
//!
//! This lives outside `d3d11.rs` on purpose. As an in-file `mod tests`, the
//! needles below would be part of the haystack and count themselves.
//!
//! Dependency-free by design: it reads the source as text and never links the
//! crate (`fastplay` is a binary, so an integration test could not import it
//! anyway).

/// The seam under inspection, embedded at compile time so the test cannot be
/// fooled by the working directory it runs from.
const D3D11_SOURCE_RAW: &str = include_str!("../src/ffi/d3d11.rs");

/// [`D3D11_SOURCE_RAW`] with line endings normalised. The working tree is CRLF
/// on Windows; these invariants are about code, not about encoding.
fn d3d11_source() -> String {
    D3D11_SOURCE_RAW.replace("\r\n", "\n")
}

/// The two sanctioned wrappers. Everything else must go through them.
const HELPERS: [&str; 2] = ["unsafe fn delete_gdi_object", "unsafe fn delete_gdi_dc"];

/// Strip `//` line comments so prose describing the old bug — including this
/// file's own subject matter, quoted in `d3d11.rs`'s helper docs — is not
/// mistaken for code.
///
/// Deliberately line-oriented: the seam uses `//` throughout, and a real block
/// comment or a `//` inside a string literal would be a false positive worth a
/// human look rather than something to silently paper over.
fn code_lines(source: &str) -> impl Iterator<Item = &str> {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(start) => &line[..start],
            None => line,
        })
        .filter(|line| !line.trim().is_empty())
}

/// Body of `name`, from its signature to the first line that closes it at the
/// function's own indentation.
fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source
        .find(name)
        .unwrap_or_else(|| panic!("{name} not found in src/ffi/d3d11.rs"));
    let rest = &source[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("no closing brace for {name}"));
    &rest[..end]
}

#[test]
fn raw_gdi_deletes_appear_only_inside_the_helpers() {
    let source = d3d11_source();
    for needle in ["DeleteObject(", "DeleteDC("] {
        let call_sites: Vec<&str> = code_lines(&source)
            .filter(|line| line.contains(needle))
            .collect();

        assert_eq!(
            call_sites.len(),
            1,
            "expected exactly one raw `{needle}` call site (the one inside its \
             helper), found {}:\n{}\n\nRoute overlay cleanup through \
             `delete_gdi_object` / `delete_gdi_dc` instead.",
            call_sites.len(),
            call_sites.join("\n")
        );

        let site = call_sites[0].trim();
        assert!(
            site.starts_with("let deleted ="),
            "the sole `{needle}` call should be the helper's own \
             `let deleted = ...`, found: {site}"
        );
    }
}

#[test]
fn helpers_delete_unconditionally_in_release_builds() {
    let source = d3d11_source();
    for helper in HELPERS {
        let body = function_body(&source, helper);
        let code: Vec<&str> = code_lines(body).collect();

        // The delete must be a plain statement. Inside `debug_assert!` it would
        // vanish from release builds — the original bug, exactly.
        let delete = code
            .iter()
            .find(|line| line.contains("let deleted ="))
            .unwrap_or_else(|| panic!("{helper} performs no delete"));
        assert!(
            !delete.contains("debug_assert!"),
            "{helper} deletes inside `debug_assert!`, so release builds skip it: {delete}"
        );

        // The assertion may stay debug-only, but it must only ever inspect the
        // already-captured result.
        for line in &code {
            if line.contains("debug_assert!") {
                assert!(
                    line.contains("deleted"),
                    "{helper} has a `debug_assert!` wrapping something other \
                     than the captured result: {line}"
                );
            }
        }

        // Guard against the helper itself being wrapped away.
        assert!(
            !body.contains("#[cfg(debug_assertions)]"),
            "{helper} is compiled out of release builds"
        );
    }
}

#[test]
fn every_helper_named_by_this_test_still_exists() {
    // Keeps the suite honest: if a helper is renamed, fail loudly here rather
    // than let the invariant tests silently inspect nothing.
    let source = d3d11_source();
    for helper in HELPERS {
        assert!(
            source.contains(helper),
            "{helper} no longer exists — update these invariants deliberately"
        );
    }
}
