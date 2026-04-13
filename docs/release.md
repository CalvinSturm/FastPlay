# Release

FastPlay release prep is version-driven:

- `Cargo.toml` controls the app version.
- `cargo wix` uses that semantic version for the MSI by default.
- GitHub release assets should match the tag and MSI filename shape:
  - tag: `vX.Y.Z`
  - MSI: `fastplay-X.Y.Z-x86_64.msi`

## Next release checklist

1. Confirm `Cargo.toml` has the intended version.
2. Build the release binary:

```powershell
cargo build --release
```

3. Build the MSI:

```powershell
cargo wix --release
```

4. Verify the output exists under `target\wix\`.
5. Smoke-test:
   - launch `target\release\fastplay.exe`
   - install/uninstall the MSI
   - confirm Start Menu shortcut
   - confirm file association open for `.mp4`
6. Create the Git tag:

```powershell
git tag vX.Y.Z
git push origin vX.Y.Z
```

7. Publish the GitHub release and upload `fastplay-X.Y.Z-x86_64.msi`.
8. Confirm the README download link matches the new tag and MSI filename.
