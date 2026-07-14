# Cutting a release

1. Bump `version` in `Cargo.toml`, run `cargo build --release` so
   `Cargo.lock` updates, and commit.
2. Tag and push:
   ```sh
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```
   `.github/workflows/release.yml` builds the binary, bundles
   completions + examples + `LICENSE`, computes `SHA256SUMS`, and
   publishes the GitHub release.
3. Refresh both AUR PKGBUILDs and push to AUR:

   **`tmux-tad`** (source build) — bump `pkgver` and refresh the source
   tarball hash:
   ```sh
   curl -sL https://github.com/ttpears/tad/archive/vX.Y.Z.tar.gz | sha256sum
   # paste into packaging/aur/tmux-tad/PKGBUILD, bump pkgver, then:
   cd packaging/aur/tmux-tad
   makepkg --printsrcinfo > .SRCINFO
   # commit + push to ssh://aur@aur.archlinux.org/tmux-tad.git
   ```

   **`tmux-tad-bin`** (prebuilt binary) — bump `pkgver` and refresh the
   release-artifact hashes:
   ```sh
   curl -sL https://github.com/ttpears/tad/releases/download/vX.Y.Z/SHA256SUMS
   # paste each hash (binary, tad.bash, _tad, groups.example,
   # config.example, LICENSE) into packaging/aur/tmux-tad-bin/PKGBUILD,
   # bump pkgver, then:
   cd packaging/aur/tmux-tad-bin
   makepkg --printsrcinfo > .SRCINFO
   # commit + push to ssh://aur@aur.archlinux.org/tmux-tad-bin.git
   ```
