# MangoLauncher

A Minecraft launcher written in Rust. Native UI (`egui`), no Electron, no
bundled browser.

> Behaviour-compatible reimplementation inspired by
> [Helios Launcher](https://github.com/dscalzi/HeliosLauncher) by Daniel Scalzi.
> No Helios code is copied; the distribution format is intentionally different.

## Status

| Milestone | Scope | State |
|---|---|---|
| M0 | workspace, config, i18n, logging, CLI skeleton | done |
| M1 | Mojang metadata, verification, downloads | done |
| M2 | Java discovery and provisioning | done |
| M3 | vanilla launch | done |
| M4 | manifest + lockfile | done |
| M5 | Fabric/Quilt/Forge/NeoForge | done |
| M6 | Microsoft + Yggdrasil auth | done (pending Mojang app review) |
| M7 | Modrinth / CurseForge mods | done |
| M8 | egui desktop UI | done |
| M9 | status, RPC, packaging, self-update | partial (server status done) |
| M10 | player-created instances | done |

Verified on Windows:

* `mango install --mc 1.20.1` fetches 3650 files (733 MB); a second run
  downloads nothing.
* `mango launch --mc 1.20.1` starts vanilla to the main menu.
* `mango launch --loader fabric` starts Fabric Loader 0.20.1 on 1.20.1.
* `mango launch --loader forge` runs the official installer headlessly and
  starts ModLauncher/Forge 47.4.28 on 1.20.1.
* `mango install --manifest examples/mods-manifest.json` resolves `sodium` and
  `iris` from Modrinth, verifies their SHA-512, and pins both in the lockfile.
  A missing optional mod is logged and skipped; a missing required mod fails the
  install before anything is downloaded.
* Fabric loads both mods: `Loading 15 mods` with `sodium 0.5.13` and
  `iris 1.7.6` in the game log.
* The launcher picks Java per instance. With only Java 22 and 25 installed, a
  1.20.1 launch downloads Temurin 17 instead of using 25, which would crash with
  `Unsupported class file major version 69`.
* `mango-launcher` renders the landing screen with the server list, live
  server status, Java picker, and a working PLAY button — around 86 MB resident
  with a manifest loaded, versus Electron's ~150 MB.
* A player-created 1.12.2 instance installs (1341 files) and launches: the
  launcher downloads Java 8 rather than reusing the installed Java 22/25, then
  reaches the main menu with LWJGL 2.9.4 initialised.
* Minecraft `26.3` (Mojang's post-`1.x` version scheme) launches on Java 25, the
  major Mojang declares in `version.json`. Nothing about the required runtime is
  hardcoded: the launcher reads `javaVersion.majorVersion` for every release.

## Desktop app

```console
cargo run -p mango-ui
```

The window has four screens: **Play** (server list, status, PLAY), **Instances**
(create/launch/delete your own instances), **Settings** (resolution, Java,
manifest, memory, mods), and **Logs**. Long-running work happens on a worker
thread; the UI never blocks and repaints when results arrive.

Set a manifest in Settings, or point `settings.launcher.manifestUrl` in
`config.json` at one. `MANGO_DATA_DIR` overrides the data directory.

## Instances

Two kinds of instance are supported, and they share one install/launch path:

* **Server packs** come from a manifest. The server owner decides the version,
  loader, and mods; the player cannot change them.
* **Your own instances** are created on the Instances screen or with the CLI.
  Pick any Minecraft release, any loader, and add mods later.

```console
mango instance versions --filter 1.20        # what can be installed
mango instance create "My Pack" --mc 1.12.2  # vanilla
mango instance create "Fabric Pack" --mc 1.20.1 --loader fabric
mango instance list
mango run fabric-pack --launch
mango instance delete fabric-pack --yes
```

In the desktop app, **Edit** on an instance reopens the same form used to create
one: the name, Minecraft version, loader, and loader version can all change. The
loader-version dropdown is populated from the loader's own API for the chosen
game version (Fabric/Quilt meta, Forge/NeoForge maven), with "newest" as the
default. Leaving the name empty falls back to the instance id.

Editing keeps the instance id, so worlds and configs in `instances/<id>/` are
preserved; the new version and loader are installed on the next launch.

Each instance lives in `instances/<id>/` with an `instance.json` recipe and a
`mango.lock.json` recording exactly which files and mod versions were installed,
so a reinstall reproduces the same setup.

Deleting an instance removes its worlds too, which is why the CLI requires
`--yes` and the UI asks for confirmation.

## Accounts

Microsoft sign-in uses OAuth 2.0 with PKCE in the system browser and a loopback
redirect, so the launcher never sees the password and there is no client secret
to leak. Tokens live in the OS keyring; `config.json` holds only metadata.

```console
mango auth status     # is a client ID configured?
mango auth login      # opens the browser (Microsoft)
mango auth login-yggdrasil --server https://auth.example.com/api/yggdrasil --username me
mango auth list       # accounts and token validity
mango auth refresh    # refresh the selected account now
mango auth logout     # remove an account and its credentials
```

Launching with a real account:

```console
mango launch --mc 1.20.1 --online      # selected account
mango run <instance> --launch          # uses the selected account automatically
```

Without `--online`, `mango launch` uses an offline profile, which is what local
testing and LAN play need.

Custom Yggdrasil servers work too: the launcher downloads authlib-injector,
attaches it as a `-javaagent`, and prefetches the server metadata so the game does
not block on a network call at startup.

Setup (Azure app registration and Mojang allow-listing) is documented in
[`docs/auth.md`](docs/auth.md).

> Sign-in fails at the last step with `HTTP 403` until Mojang approves the
> client ID. The launcher reports this specifically rather than as a generic
> failure.

## Layout

```
crates/mango-core   all launcher logic (no UI dependency)
crates/mango-cli    `mango` command line tool
crates/mango-ui     `mango-launcher` desktop app
```

## Development

Requires Rust 1.85+ and, on Windows, the MSVC build tools.

```console
cargo build --workspace
cargo test --workspace
cargo run -p mango-cli -- --help
```

### CLI

The launcher data directory defaults to the platform data dir and can be
overridden with `--data-dir` or `MANGO_DATA_DIR`.

```console
cargo run -p mango-cli -- info
cargo run -p mango-cli -- install --mc 1.20.1
cargo run -p mango-cli -- install --manifest ./examples/manifest.json --locked
cargo run -p mango-cli -- java list
cargo run -p mango-cli -- java install 17
cargo run -p mango-cli -- launch --mc 1.20.1 --username Steve --dry-run
cargo run -p mango-cli -- launch --mc 1.20.1 --loader fabric --attached
cargo run -p mango-cli -- launch --mc 1.20.1 --loader forge --attached
```

The manifest format is documented in [`docs/manifest.md`](docs/manifest.md).

Forge and NeoForge are installed by running their official installer headlessly,
which needs a JVM on `PATH` or `MANGO_JAVA` pointing at one; use Java 17+ for
Minecraft 1.17 and newer.

## License

MIT. Minecraft assets are downloaded from Mojang's official CDNs and are never
redistributed.
