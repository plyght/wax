# Investigation plan: Wax as a winget + scoop stand-in on Windows

This document is a **research and spike plan** (not an implementation spec). It
outlines how to study **Windows Package Manager (winget)** and **Scoop** so
Windows builds of wax can credibly replace them: same user workflows, comparable
coverage assumptions, and clear boundaries where behaviour must differ.

[UNIFIED_SOURCES.md](windows-notes/UNIFIED_SOURCES.md) documents how wax maps winget/Scoop/Chocolatey-style sources
today; this plan adds **platform reality** (installers, sources, shims) and
**Scoop**, which winget-only parity work does not cover.

---

## 1. Goals and success criteria

**Investigation is done when** we can answer, with evidence (links, notes, small
proof-of-concept commands):

| Question | Why it matters |
|----------|----------------|
| Which operations must run **in-process** vs **shelling out** to `winget.exe` / PowerShell? | Avoids brittle coupling while respecting elevation and store policies. |
| What **manifest / index** formats and versions does each tool use end-to-end? | Drives whether wax parses YAML/JSON directly or consumes a normalized index. |
| How do **shims**, **PATH**, and **persisted data** work for Scoop vs winget? | Matches user expectations on upgrade/uninstall. |
| What **installer types** (MSI, EXE with silent flags, MSIX, portable ZIP) are common per ecosystem? | Scoop and winget handle these differently; wax must pick a strategy per type. |
| What **sources** exist by default (winget `winget`, `msstore`; Scoop main bucket + buckets) and how are they trusted? | Informs wax “default registry” design on Windows. |
| Where do **elevation**, **UAC**, and **Microsoft Store** block unattended flows? | Documents honest limitations for `wax install` on Windows. |

---

## 2. Phase 0 — Baseline environment (half day)

- [ ] **P0.1** Install current **winget** (App Installer) and record version:
  `winget --info`, `winget source list`, `winget features` (if available).
- [ ] **P0.2** Install **Scoop** per official instructions; record install root,
  `scoop bucket list`, and one representative app + shim path.
- [ ] **P0.3** Capture `%PATH%` before/after for both tools; note `~\scoop\shims`
  vs winget’s execution model (no universal shim directory).
- [ ] **P0.4** Decide where investigation notes live (this repo:
  `docs/windows-notes/` or linked scratchpad) so findings stay traceable.

---

## 3. Phase 1 — Winget deep dive

Work through official and source materials in this order:

### 3.1 Client and documentation

- [ ] **W1.1** Read Microsoft docs: Windows Package Manager overview, manifest
  schema (1.0 vs 1.1+), installer types, and `winget` command reference.
- [ ] **W1.2** Clone [microsoft/winget-cli](https://github.com/microsoft/winget-cli)
  locally; locate: source resolution, install execution, COM registration
  (`winget.exe` vs `AppInstaller` package), and how `msstore` IDs are handled.
- [ ] **W1.3** Document the **REST source contract** used by private feeds
  (relation to [winget-cli-restsource](https://github.com/microsoft/winget-cli-restsource)).

### 3.2 Behaviour experiments (manual)

- [ ] **W1.4** `winget show` / `winget install` for: MSI app, installer EXE with
  silent switches, zip/portable style (if available in public manifests).
- [ ] **W1.5** `winget export` → JSON structure; note fields wax would need for
  round-trip (`PackageIdentifier`, `Source`, version pinning semantics).
- [ ] **W1.6** Record failure modes: elevation denied, dependency package,
  reboot required, interactive installer (no silent path).

### 3.3 Deliverable

- [ ] **W1.7** One-page **“winget execution model”**: from resolved manifest to
  launched installer and registered uninstall entry.

---

## 4. Phase 2 — Scoop deep dive

Scoop is **Git buckets + JSON manifests + PowerShell installer scripts + shims**.
Investigation should not assume winget’s YAML model covers it.

### 4.1 Repository structure

- [ ] **S2.1** Read [ScoopInstaller/Scoop](https://github.com/ScoopInstaller/Scoop)
  docs: architecture, `scoop install` flow, shim generation.
- [ ] **S2.2** Inspect **Main bucket** (or Main on GitHub): typical `*.json`
  manifest fields (`url`, `hash`, `bin`, `env_set`, `persist`, `installer`).

### 4.2 Behaviour experiments

- [ ] **S2.3** Install a **console** tool and a **GUI** tool; diff shim vs
  shortcut behaviour.
- [ ] **S2.4** `scoop list` / `scoop status`; understand hold/pin vs winget pin.
- [ ] **S2.5** Note **anti-virus / Defender** interactions (Scoop often runs user-local
  installs; still document observed prompts).

### 4.3 Deliverable

- [ ] **S2.6** One-page **“Scoop manifest minimal schema”** (what wax must parse
  for parity on a first bucket) vs optional fields (extras).

---

## 5. Phase 3 — Comparative matrix (winget vs Scoop vs wax on Windows)

Fill a table (spreadsheet or markdown) with **rows = user intent**, **columns =
tool**:

| Intent | winget | Scoop | wax (today on Windows) | wax (target after work) |
|--------|--------|-------|-------------------------|-------------------------|
| Search | … | … | … | TBD |
| Install portable ZIP | … | … | … | TBD |
| Install with UAC / machine scope | … | … | … | TBD |
| List upgradable | … | … | … | TBD |
| Pin version | … | … | … | TBD |
| Multiple sources / buckets | … | … | … | TBD |
| Export / import package set | … | … | … | TBD |

- [ ] **C3.1** Complete matrix with **citations** to manifest fields or code paths.
- [ ] **C3.2** Mark cells **non-goal** where policy forbids parity (e.g. certain
  Store-only packages).

---

## 6. Phase 4 — Integration hypotheses for wax (design spikes)

These are *candidates* to validate or reject after Phases 1–3.

| Hypothesis | Spike | Outcome |
|------------|-------|---------|
| Wax consumes **winget REST** + optional local index cache | Small Rust client to fetch package by id and parse installer nodes | Feasibility + rate limits |
| Wax reads **Scoop bucket JSON** as an alternate source | Parse Main bucket entry, resolve URL + hash, download | Alignment with existing tap/Git model |
| Unified **shim layer** on Windows | Create `~/.local/bin`-style directory + `.exe` shims or hardlinks | Match Scoop UX without duplicating PowerShell |
| **Delegation fallback** | Optional `wax install --via winget` (explicit opt-in only) | Reduces friction for MSIX-only packages without making delegation the default story |

- [ ] **I4.1** Run **at least two spikes** above and record **go / no-go** with
  complexity estimate.

---

## 7. Phase 5 — Risks, non-goals, compliance

- [ ] **R5.1** **Licensing**: redistributing manifests, using Microsoft trademarks,
  Store package rules — short note from docs or legal review if needed.
- [ ] **R5.2** **Security**: hash verification policy, signature expectations for
  EXE installers vs Scoop’s model.
- [ ] **R5.3** **Non-goals (initial)**: list things wax may *not* replicate in v1
  (e.g. full COM API of App Installer, full MS Store purchase flow).

---

## 8. Suggested timeline (indicative)

| Week | Focus |
|------|--------|
| 1 | Phase 0–1 (winget docs + experiments + execution model note) |
| 2 | Phase 2 (Scoop) + start comparative matrix |
| 3 | Complete matrix + two integration spikes (Phase 4) |
| 4 | Risks, non-goals, prioritized backlog tied to [UNIFIED_SOURCES.md](windows-notes/UNIFIED_SOURCES.md) |

Adjust based on maintainer bandwidth.

---

## 9. References (starting points)

- Windows Package Manager: `https://learn.microsoft.com/windows/package-manager/`
- winget-cli repository: `https://github.com/microsoft/winget-cli`
- Manifest schema: `https://github.com/microsoft/winget-pkgs` (community manifests)
- Scoop: `https://github.com/ScoopInstaller/Scoop`
- Scoop Main bucket: `https://github.com/ScoopInstaller/Main`

---

## 10. Relation to unified source support

- [UNIFIED_SOURCES.md](windows-notes/UNIFIED_SOURCES.md): **bang prefixes**, **auto
  source pick** (fastest wins), and **installer scope** for brew / Scoop / winget-pkgs /
  Chocolatey.
- **This document**: **investigation methodology** for Windows, **Scoop**, and
  **installer/source reality** so “stand-in” is grounded in behaviour, not only
  CLI name matching.

After investigation, extend **UNIFIED_SOURCES.md** with measured Windows-native
install paths and any policy that changes which installer families wax handles in-process.

---

## 11. Desk research (Linux / CI)

When a Windows host is not available, use [windows-notes/DESK_RESEARCH.md](windows-notes/DESK_RESEARCH.md)
for documentation-derived notes: winget execution outline, REST source pointer,
Scoop JSON minimal fields, and a **partial** comparative matrix. **Phase 0–2
checkboxes in §§2–4 remain open** until commands are run on Windows and outputs
are captured (see §2 P0.4 for where to store them).
