# Releasing CC Codex Proxy

This project publishes macOS app releases from immutable version tags. The release workflow builds the app bundle, packages a drag-to-Applications DMG, verifies the artifacts, creates artifact attestations, and publishes a GitHub Release.

## Version policy

- Stable releases use SemVer-style tags: `vMAJOR.MINOR.PATCH`.
- The tag is the source of truth for public releases.
- The package versions and app plist should match the release tag before tagging:
  - `crates/cc-codex-proxy/Cargo.toml`
  - `crates/proxy-core/Cargo.toml`
  - `Cargo.lock`
  - `macos/CCCodexProxy/Info.plist`
- Homebrew formula/cask versions under `Formula/`, `Casks/`, and `packaging/homebrew/` should match the release tag. Homebrew checksums are finalized after the GitHub tag source archive and release DMG are available.
- Do not replace assets on an already-published stable version. If an artifact changes, publish a new tag.

Suggested bump rules:

- Patch: bug fixes, documentation corrections, release pipeline fixes.
- Minor: user-visible app features or compatibility improvements.
- Major: breaking workflow, config, or compatibility changes.

## Release assets

Each release should contain versioned assets plus stable latest aliases:

- `CCCodexProxy-<version>-macOS.dmg`
- `CCCodexProxy-<version>-macOS.zip`
- `CCCodexProxy-macOS.dmg`
- `CCCodexProxy-macOS.zip`
- `SHA256SUMS`
- `RELEASE_MANIFEST.json`

The versioned assets are the immutable source of truth. The stable aliases exist so the README can link to the latest DMG with a URL that does not change between releases.

## Release workflow

The tag-triggered workflow in `.github/workflows/release.yml` should:

1. Validate that the tag version matches package/app metadata.
2. Run Rust tests.
3. Build the Swift app.
4. Build the DMG/ZIP/checksum/manifest artifacts with `scripts/build-app.sh`.
5. Verify the DMGs with `hdiutil verify`.
6. Verify checksums with `shasum -a 256 -c SHA256SUMS`.
7. Generate GitHub artifact attestations with `actions/attest`.
8. Create a draft GitHub Release, upload all assets, then publish it.

Required workflow permissions are intentionally narrow for the job:

- `contents: write` to publish the release.
- `id-token: write` and `attestations: write` to create artifact attestations.

Regular CI in `.github/workflows/ci.yml` keeps `contents: read` and runs tests plus a packaging smoke test on pull requests and pushes to `main`.

## Local preflight

Before tagging, run:

```sh
cargo test --all
swift build --package-path macos/CCCodexProxy
CCP_VERSION=<version> CCP_BUILD_NUMBER=1 scripts/build-app.sh
hdiutil verify dist/CCCodexProxy-<version>-macOS.dmg
hdiutil verify dist/CCCodexProxy-macOS.dmg
(cd dist && shasum -a 256 -c SHA256SUMS)
```

## Publish a release

After the release commit is ready locally:

```sh
git push origin main
git tag v<version>
git push origin v<version>
gh run watch --workflow release.yml --exit-status
gh release view v<version> --web
```

## Update Homebrew metadata

After the GitHub Release is published, update the Homebrew formula and cask hashes from the immutable tag source archive and release checksum file:

```sh
VERSION=<version>
curl -L -o "/tmp/cc-codex-proxy-v$VERSION.tar.gz" "https://github.com/soulforger0/cc-codex-proxy/archive/refs/tags/v$VERSION.tar.gz"
curl -L -o /tmp/cc-codex-proxy-SHA256SUMS "https://github.com/soulforger0/cc-codex-proxy/releases/download/v$VERSION/SHA256SUMS"
SOURCE_SHA256="$(shasum -a 256 "/tmp/cc-codex-proxy-v$VERSION.tar.gz" | awk '{print $1}')"
DMG_SHA256="$(awk -v artifact="CCCodexProxy-$VERSION-macOS.dmg" '$2 == artifact {print $1}' /tmp/cc-codex-proxy-SHA256SUMS)"
scripts/update-homebrew-packaging.sh "$VERSION" "$SOURCE_SHA256" "$DMG_SHA256"
```

Validate the metadata before publishing the Homebrew update:

```sh
ruby -c Formula/cc-codex-proxy.rb
ruby -c Casks/cc-codex-proxy-app.rb
ruby -c packaging/homebrew/cc-codex-proxy.rb
ruby -c packaging/homebrew/cc-codex-proxy-app.rb
```

After the Homebrew metadata is committed and visible through a tap, validate by tap-qualified name:

```sh
brew tap soulforger0/cc-codex-proxy https://github.com/soulforger0/cc-codex-proxy
brew audit --strict --formula soulforger0/cc-codex-proxy/cc-codex-proxy
brew audit --strict --cask soulforger0/cc-codex-proxy/cc-codex-proxy-app
brew install --formula --dry-run soulforger0/cc-codex-proxy/cc-codex-proxy
brew install --cask --dry-run soulforger0/cc-codex-proxy/cc-codex-proxy-app
```

## Verify a downloaded release

After downloading the DMG and `SHA256SUMS` from a release:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing
gh attestation verify CCCodexProxy-macOS.dmg --repo soulforger0/cc-codex-proxy
```

## Signing and notarization roadmap

Current releases are ad-hoc signed but not Developer ID signed or notarized. Users may need to right-click the app and choose **Open** on first launch.

The recommended future production setup is:

1. Store Developer ID certificate and Apple notarization credentials as GitHub Actions secrets.
2. Sign the app and helper with a Developer ID Application certificate.
3. Package the signed app into the DMG.
4. Submit the DMG to Apple notarization.
5. Staple the notarization ticket when applicable.
6. Update the README and release notes to remove the unsigned-build warning.

Until then, every release note and install section should clearly state that builds are ad-hoc signed and not notarized.
