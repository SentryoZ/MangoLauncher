# Manifest format

A manifest tells MangoLauncher what to install and launch. It is served over
HTTP (or read from disk during development) and cached locally so the launcher
still starts when the host is unreachable.

This format is intentionally **not** compatible with Helios'
`distribution.json`: mods are referenced by their upstream project rather than
hosted by the launcher, and loader metadata comes from each loader's own API.

## Example

```json
{
  "schemaVersion": 1,
  "name": "Mango Network",
  "updatedAt": "2026-11-09T00:00:00Z",
  "news": { "rss": "https://example.com/news.rss" },
  "discord": {
    "clientId": "123456789012345678",
    "largeImageKey": "mango-large",
    "largeImageText": "Mango Network",
    "smallImageKey": "mango-small",
    "smallImageText": "mangolauncher"
  },
  "servers": [
    {
      "id": "main",
      "name": "Mango Survival",
      "description": "The main survival server.",
      "icon": "https://example.com/icons/main.png",
      "address": "play.example.com:25565",
      "minecraft": "1.20.1",
      "mainServer": true,
      "autoConnect": true,
      "loader": { "type": "fabric", "version": "0.16.9" },
      "java": {
        "ram": { "recommended": 4096, "minimum": 2048 }
      },
      "jvmArgs": ["-XX:+UseG1GC"],
      "mods": [
        { "source": "modrinth", "id": "sodium", "version": "latest", "required": true }
      ],
      "files": [
        {
          "path": "config/mango.cfg",
          "url": "https://cdn.example.com/main/config/mango.cfg",
          "size": 1234,
          "sha256": "…",
          "overwrite": false
        }
      ]
    }
  ]
}
```

## Top level

| Field | Required | Meaning |
|---|---|---|
| `schemaVersion` | yes | Must be `1`. Older launchers reject newer manifests instead of misreading them. |
| `name` | yes | Shown in the UI and in logs. |
| `servers` | yes | At least one entry. |
| `updatedAt` | no | ISO-8601 timestamp. Used to detect lockfile drift. |
| `news` | no | `{ "rss": "<url>" }`. |
| `discord` | no | Rich-presence artwork settings. |

Unknown fields are rejected so typos fail loudly instead of being ignored.

## Server entry

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | Stable identifier. Renaming it orphans the player's instance directory. |
| `name` | yes | Display name. |
| `address` | yes | `host` or `host:port`. Used for autoconnect and the status ping. |
| `minecraft` | yes | Minecraft version id, e.g. `1.20.1`. |
| `loader` | no | `{ "type": "vanilla" \| "fabric" \| "quilt" \| "forge" \| "neoforge", "version": "…" }`. Defaults to vanilla. Omit `version` to resolve the newest compatible build. |
| `java` | no | Java requirements, see below. |
| `jvmArgs` | no | Extra JVM flags appended to the launcher's defaults. |
| `mods` | no | Upstream mod references, see below. |
| `files` | no | Loose files placed inside the instance directory. |
| `mainServer` | no | Exactly one server may set this; it becomes the default selection. |
| `autoConnect` | no | Join `address` on launch. |

## Java

| Field | Meaning |
|---|---|
| `ram.recommended` / `ram.minimum` | Slider defaults, in megabytes. |

There is deliberately no way to pin the Java version here. The requirement comes
from `javaVersion.majorVersion` in the release's own `version.json`, which Mojang
publishes for every version including the oldest ones (1.7.10 declares 8). A
manifest that repeated it could only be wrong, and a wrong value is not a
cosmetic problem — see below.

## Mods

```json
{ "source": "modrinth", "id": "sodium", "version": "latest", "required": true }
{ "source": "curseforge", "id": 238222, "fileId": 4567890, "required": false, "disabledByDefault": true }
```

* `id` accepts a Modrinth slug or a numeric CurseForge project id.
* `version` is an exact upstream version id, or `latest` to resolve against the
  server's `minecraft` version and loader.
* `required: false` lets the player toggle the mod; `disabledByDefault` starts
  it disabled. **Required mods cannot be disabled** — the server disconnects a
  client that is missing them.
* Mods are downloaded from the catalogue's CDN and verified by SHA-512 (Modrinth)
  or SHA-1 (CurseForge), then recorded in the lockfile.
* CurseForge entries need an API key (`x-api-key`), which CurseForge only grants
  after manual review. Without a key those entries are skipped with a warning, so
  Modrinth should be the default choice.

## Java per instance

The required Java major comes from `javaVersion.majorVersion` in the release's
`version.json`, which is authoritative for every version scheme. Mojang moved
from `1.21.x` to plain `26.3`-style names, and those names carry no information
about the runtime — `26.3` needs Java 25 while `1.20.1` needs 17 — so the version
number must never be used to guess.

The launcher requires that exact major rather than "at least", because running
Minecraft on a newer JVM than it was compiled for fails at class initialisation:

```text
UnsupportedClassVersionError: net/minecraft/client/main/Main has been compiled
by a more recent version of the Java Runtime (class file version 69.0), this
version of the Java Runtime only recognizes class file versions up to 65.0
```

When no local runtime matches, the matching Temurin JDK is downloaded
automatically. There is no override: a manifest or instance that disagreed with
Mojang would reintroduce exactly this crash, and a server owner has no better
information about which runtime a release was compiled for.

### Native libraries

Legacy versions expect native jars extracted into `-Djava.library.path`.
Modern versions (`1.19`+ with the `java.library.path=${natives_directory}/java`
template) instead give LWJGL its own extract directory, so the natives jars must
be on the classpath and the `java`, `jna`, `lwjgl`, and `netty` subdirectories
must exist before launch. Both layouts are prepared on every launch.

## Files

Paths are relative to the instance directory and must not be absolute or contain
`..`. `overwrite: false` means an existing file is left alone, so a config the
player edited survives a reinstall; `overwrite: true` enforces the hash.

## Lockfile

Installing writes `instances/<server>/mango.lock.json`, which pins every file and
resolved mod version:

```json
{
  "schemaVersion": 1,
  "server": "main",
  "manifestVersion": "2026-11-09T00:00:00Z",
  "minecraft": "1.20.1",
  "loader": { "type": "fabric", "version": "0.16.9" },
  "versionUrl": "https://piston-meta.mojang.com/v1/packages/…/1.20.1.json",
  "files": [
    {
      "kind": "library",
      "scope": "common",
      "path": "libraries/org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar",
      "url": "https://example.com/lwjgl.jar",
      "size": 10,
      "sha1": "…"
    }
  ],
  "mods": []
}
```

`scope` is `common` or `instance`, so a lockfile stays valid if only one of the
two directories is relocated. `mango install --locked` refuses to run when no
lockfile matches the manifest revision.
