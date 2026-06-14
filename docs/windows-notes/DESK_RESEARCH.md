# Desk research: winget, Scoop, and wax (no Windows host)

This file records **documentation-based** findings for
[WINDOWS_PACKAGE_MANAGER_INVESTIGATION.md](../WINDOWS_PACKAGE_MANAGER_INVESTIGATION.md)
while development runs on Linux/macOS CI. Anything marked **needs machine**
requires a Windows workstation to verify versions, PATH, and installer behaviour.

## P0 baseline (needs machine)

- **P0.1–P0.4**: Run `winget --info`, `winget source list`, install Scoop, capture
  `%PATH%`, and archive command output under this folder or the issue/PR.

## Winget execution model (W1.7 — desk summary)

1. **Resolution**: The client resolves a package id + source (`winget`, `msstore`,
   or a configured REST source) to a **manifest** (YAML; schema versions 1.0+).
2. **Installer graph**: The manifest lists one or more **installers** (MSI, EXE,
   MSIX, zip/portable, etc.) with scope, architecture, and locale selection.
3. **Acquisition**: The client downloads the selected installer (or archive) to a
   cache location, verifies hash where specified, and may invoke **Microsoft Store**
   or **App Installer** flows for `msstore` / MSIX.
4. **Execution**: Non-store flows typically run the installer with documented
   silent switches; outcomes are registered for **Apps & Features** / uninstall.
5. **Elevation**: Many installers require UAC; unattended use fails if elevation
   is denied (document as a limitation for wax on Windows).

Official overview: [Windows Package Manager](https://learn.microsoft.com/windows/package-manager/).

## REST source contract (W1.3 — desk summary)

Private feeds implement the **winget REST source** API (OpenAPI-oriented contract;
reference implementation: [winget-cli-restsource](https://github.com/microsoft/winget-cli-restsource)).
Future REST sources (see [UNIFIED_SOURCES.md](UNIFIED_SOURCES.md)) may target compatibility with
that contract behind `rest-sources`.

## Scoop manifest minimal schema (S2.6 — desk summary)

Scoop buckets ship **JSON** manifests (not winget YAML). A **minimal** first-pass
schema for parity discussions:

| Field | Role |
|--------|------|
| `version` | Installed version string |
| `url` / `hash` | Artifact download and integrity |
| `bin` | Executable(s) exposed via shims |
| `extract_dir` / `extract_to` | Archive layout (when applicable) |
| `installer` / `pre_install` / `post_install` | Scripted install steps (PowerShell) |
| `persist` | User data kept across upgrades |
| `depends` | Bucket-local dependencies |

Scoop installs are user-local under the Scoop root; **shims** in `~\scoop\shims`
are the primary PATH integration (contrast with winget’s installer-centric model).

## Comparative matrix (C3.1 — partial, desk-only)

| Intent | winget (typical) | Scoop (typical) | wax (Unix today) | wax Windows target |
|--------|------------------|-----------------|------------------|-------------------|
| Search | Index + REST / local cache | `scoop search` over bucket JSON | Formula/cask index | TBD — align with tap + future REST |
| Install portable ZIP | Manifest `InstallerType: zip` | Common in JSON manifests | Bottles / cask artifacts | TBD |
| Machine scope / UAC | Often via MSI/EXE installers | Rare (user scope) | `wax install --global` (Unix) | TBD — policy + elevation |
| List upgradable | `upgrade --include-unknown` / list | `scoop status` | `wax list --upgradable` | Same verb goal |
| Pin | `pin add/remove/list` | `scoop hold` | `wax pin` / `pin list` | Same verb goal |
| Extra sources | `source add` (incl. REST) | `scoop bucket add` | `wax tap` | + REST (`rest-sources`) |
| Export / import | JSON package list | N/A (custom) | Waxfile; winget JSON planned | Roadmap in repo |

Cells marked **TBD** should be filled after P0–P2 experiments on Windows.

## Integration spikes (I4.1)

No spikes were executed in this branch from Linux; recommend **two** of: REST
client probe, sample Scoop JSON parse, shim directory prototype, or explicit
`--via winget` delegation — as listed in the investigation doc.

## Relation to repo plans

- CLI / source UX: see [UNIFIED_SOURCES.md](UNIFIED_SOURCES.md) for bangs and auto pick.
- Platform depth: revise **UNIFIED_SOURCES.md** after Windows evidence is collected (per investigation §10).
