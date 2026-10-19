# Releasing NucleOS

A release is a `v*` tag. Pushing it runs `.github/workflows/release.yml`, which builds the
shell + `nucleos-core` + the Go sidecars on Windows, macOS and Linux, signs them, checks that the
draft is complete, adds `SHA256SUMS`, and publishes it. Nothing below contains key material:
gitleaks runs in CI, and the keys never enter git.

## One version

`VERSION` is the single source. `scripts/bump-version.py` writes it to the seven places that carry
a version (`core/Cargo.toml`, `shell/src-tauri/Cargo.toml`, `shell/package.json`,
`shell/src-tauri/tauri.conf.json`, `Cargo.lock`, `shell/src-tauri/Cargo.lock`,
`shell/package-lock.json`), and `--check` fails naming the file that drifted. `scripts/gates.sh
hooks` runs the check on every commit, and the release workflow runs it against the tag before
building anything.

## One-time setup

### Updater key pair

The updater verifies every download against a minisign public key baked into the app.

```bash
npm --prefix shell run tauri -- signer generate -w ~/.tauri/nucleos-updater.key
```

Choose a password and keep an offline copy of the key file and the password. Losing the key means
no installed copy can ever be updated again; leaking it means anyone can ship an update.

- The private key file's content becomes the secret `TAURI_SIGNING_PRIVATE_KEY`, the password
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
- The printed `.pub` is public. It goes into `plugins.updater.pubkey` in
  `shell/src-tauri/tauri.conf.json`, replacing the `REPLACE_WITH_...` placeholder. The release
  workflow refuses to run while that placeholder is there.

### macOS certificate

macOS ties the microphone and accessibility permissions (voice, dictation) to the code-signing
identity. An ad-hoc signature changes with every build, so every update would ask again. A stable
self-signed certificate costs nothing and keeps the identity across releases. Run once (Git for
Windows ships `openssl`):

```bash
openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 -nodes \
  -keyout nucleos-macos.key -out nucleos-macos.crt -subj "/CN=NucleOS Self-Signed" \
  -addext "basicConstraints=critical,CA:false" -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning"
openssl pkcs12 -export -inkey nucleos-macos.key -in nucleos-macos.crt -name "NucleOS Self-Signed" \
  -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1 -out nucleos-macos.p12
base64 -w0 nucleos-macos.p12
```

The legacy `-certpbe/-keypbe/-macalg` options are what lets macOS `security import` read the file.
The certificate is valid for ten years; renewing it changes its hash and every permission is asked
again once. Keep the `.key`, the `.p12` and its password offline.

### Repository secrets

GitHub, Settings, Secrets and variables, Actions:

| Secret | Value |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | content of the updater private key file |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | its password |
| `APPLE_CERTIFICATE` | the output of `base64 -w0 nucleos-macos.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_SIGNING_IDENTITY` | `NucleOS Self-Signed` |

The workflow fails closed: with an empty updater key, or an empty certificate on macOS, the build
stops with an error instead of producing an unsigned or ad-hoc signed installer. After the macOS
build it also checks that `codesign` shows that identity and a certificate-anchored designated
requirement.

## Cutting a release

```bash
python scripts/bump-version.py X.Y.Z
git commit -am "chore: release X.Y.Z"
git tag vX.Y.Z
git push origin master vX.Y.Z
```

The tag must equal `VERSION`; the workflow checks it. Watch the run: the `publish` job downloads
the assets the three legs uploaded, `scripts/release-manifest.py verify` checks that each platform
brought its installer, its `.sig` (where the updater needs one) and the right version, `sums` writes
`SHA256SUMS`, and only then is the draft made public. If a local `pre-push` hook refuses the push,
the escape is `NUCLEOS_ALLOW_DIRECT_GIT=1 git push origin vX.Y.Z`.

macOS minutes bill at ten times while the repository is private, which is why this workflow runs
only on a tag.
