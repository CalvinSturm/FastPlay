# Release

FastPlay release prep is version-driven:

- `Cargo.toml` controls the app version.
- `cargo wix` uses that semantic version for the MSI by default.
- GitHub release assets should match the tag and MSI filename shape:
  - tag: `vX.Y.Z`
  - MSI: `fastplay-X.Y.Z-x86_64.msi`
  - portable ZIP: `fastplay-X.Y.Z-windows-x86_64-portable.zip`

## Next release checklist

1. Confirm `Cargo.toml` has the intended version.
2. Build the release binary:

```powershell
cargo build --release
```

3. Build the MSI:

```powershell
cargo wix
```

4. Verify the output exists under `target\wix\`.
5. Build the portable ZIP from the same release output:

```powershell
.\scripts\package-portable.ps1 -SkipBuild
```

6. Verify the ZIP exists under `target\dist\` and contains one versioned
   directory with `fastplay.exe`, the MSI's runtime DLL set, the portable
   README, and license notices.
7. Smoke-test:
   - launch `target\release\fastplay.exe`
   - extract the portable ZIP to a clean directory and launch its
     `fastplay.exe` without installing FastPlay
   - open representative hardware- and software-decoded media from the
     extracted bundle
   - install/uninstall the MSI
   - confirm Start Menu shortcut
   - confirm file association open for `.mp4`
8. Create the Git tag:

```powershell
git tag vX.Y.Z
git push origin vX.Y.Z
```

9. Publish the GitHub release and upload both
   `fastplay-X.Y.Z-x86_64.msi` and
   `fastplay-X.Y.Z-windows-x86_64-portable.zip`.
10. Confirm the README download links match the new tag and both asset names.
