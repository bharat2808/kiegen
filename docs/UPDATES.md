# Application updates

Kiegen uses Tauri's signed updater with a static manifest attached to GitHub Releases.
The app checks only when the user selects **Check for Updates** in Settings → General.
An available update shows its version and offers an explicit install action; installing
downloads the signed archive, replaces the app, and relaunches it.

## Signing keys

The updater keypair is separate from Apple's Developer ID certificate. The public key is
in `src-tauri/tauri.conf.json`. The private key must stay outside the repository and must
be backed up securely: existing installations trust that key, so losing it prevents future
updates. On the release Mac, the local key is stored at `~/.tauri/kiegen-updater.key` with
owner-only file permissions. Never attach it to a release or commit it.

## Build and publish an Apple Silicon update

1. Increase the app version in `package.json`, `src-tauri/Cargo.toml`, and
   `src-tauri/tauri.conf.json`; keep `package-lock.json` and `src-tauri/Cargo.lock` in sync.
2. Build with the updater key available to Tauri:

   ```sh
   export TAURI_SIGNING_PRIVATE_KEY_PATH="$HOME/.tauri/kiegen-updater.key"
   npm run tauri build -- --bundles app
   ```

   The bundle directory contains `kiegen.app`, `kiegen.app.tar.gz`, and
   `kiegen.app.tar.gz.sig`. Keep the archive and its signature paired.
3. Sign and notarize the app with the Bharat Developer ID, then staple its ticket. If
   stapling changes the app after Tauri produced the updater archive, rebuild the archive
   from the stapled app and re-sign it:

   ```sh
   npm run tauri signer sign \
     -- --private-key-path "$HOME/.tauri/kiegen-updater.key" \
     --app-version 0.1.2 \
     src-tauri/target/release/bundle/macos/kiegen.app.tar.gz
   ```

   Verify that the `.sig` file next to the archive is refreshed before publishing.
4. Upload the app archive and signature as assets on the versioned GitHub Release. Generate
   `latest.json` using the helper below, then upload it to the same release as an asset
   named `latest.json`:

   ```sh
   node scripts/write-updater-manifest.mjs \
     0.1.2 \
     https://github.com/bharat2808/kiegen/releases/download/v0.1.2/kiegen.app.tar.gz \
     src-tauri/target/release/bundle/macos/kiegen.app.tar.gz.sig \
     latest.json
   ```

The configured updater endpoint always fetches `latest.json` from the latest GitHub
Release. The manifest currently advertises `darwin-aarch64`; add an independently built
and signed `darwin-x86_64` artifact before offering Intel Mac updates.

The already installed v0.1.1 app was built before the updater was added. It cannot update
itself; install the first updater-enabled DMG once, then later releases can arrive through
the in-app update flow.
