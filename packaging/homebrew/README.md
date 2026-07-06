# Homebrew Packaging

This project ships two Homebrew definitions:

- `Formula/cc-codex-proxy.rb` builds and installs the Rust CLI helper from a tagged source archive.
- `Casks/cc-codex-proxy-app.rb` installs the prebuilt Apple Silicon (`arm64`) macOS menu bar app from the release DMG.

The matching files under `packaging/homebrew/` are kept as release-maintained copies for packaging review and tap publication workflows.

## Install From This Repository

Until a dedicated `soulforger0/homebrew-cc-codex-proxy` tap exists, users can tap this source repository by URL:

```sh
brew tap soulforger0/cc-codex-proxy https://github.com/soulforger0/cc-codex-proxy
brew install --cask soulforger0/cc-codex-proxy/cc-codex-proxy-app
```

The app cask is restricted with `depends_on arch: :arm64` because current release DMGs are native arm64 builds, not universal builds.

The CLI-only helper can be installed with:

```sh
brew install soulforger0/cc-codex-proxy/cc-codex-proxy
```

Homebrew 6 expects formulae and casks to be installed or audited from a tap. For local testing after committing packaging changes on a branch:

```sh
brew tap local/cc-codex-proxy "$(pwd)"
brew audit --strict --formula local/cc-codex-proxy/cc-codex-proxy
brew audit --strict --cask local/cc-codex-proxy/cc-codex-proxy-app
brew install --formula --dry-run local/cc-codex-proxy/cc-codex-proxy
brew install --cask --dry-run local/cc-codex-proxy/cc-codex-proxy-app
brew untap local/cc-codex-proxy
```

## Release Updates

After publishing a tag and release assets, update both tap-visible and packaging copies:

```sh
scripts/update-homebrew-packaging.sh <version> <source_sha256> <dmg_sha256>
```

`source_sha256` is the hash of `https://github.com/soulforger0/cc-codex-proxy/archive/refs/tags/v<version>.tar.gz`.
`dmg_sha256` is the hash for the arm64 `CCCodexProxy-<version>-macOS.dmg` from the release `SHA256SUMS`.

Then validate the Ruby files:

```sh
ruby -c Formula/cc-codex-proxy.rb
ruby -c Casks/cc-codex-proxy-app.rb
ruby -c packaging/homebrew/cc-codex-proxy.rb
ruby -c packaging/homebrew/cc-codex-proxy-app.rb
```

After the metadata is published in a tap, validate the Homebrew DSL by tap-qualified name:

```sh
brew audit --strict --formula soulforger0/cc-codex-proxy/cc-codex-proxy
brew audit --strict --cask soulforger0/cc-codex-proxy/cc-codex-proxy-app
```
