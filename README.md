# piCoreCDSP — CamillaDSP-native architecture (v2)

## Status: v2 in development — v1 archived

piCoreCDSP is being rebuilt from scratch as **v2**: a small, CamillaDSP-native
controller built directly against CamillaDSP's own process/WebSocket/ALSA
model, rather than a continuation of the previous dual-backend
(`snd-aloop` + custom `ioplug`) controller.

As of this change, `main` contains **only** the v2 planning/architecture
documents. There is no installable piCoreCDSP build on `main` yet — the v1
Rust controller, the native `picoredsp-ioplug` ALSA plugin, the installer,
benchmarks, and their CI have all been removed from `main`'s working tree as
part of the deliberate, one-way Gate 0 cutover described in the roadmap below.
Nothing is lost: the full v1 implementation remains permanently available via
git.

## Where to look

- [`piCoreCDSP_v2_Roadmap.md`](piCoreCDSP_v2_Roadmap.md) — the v2 roadmap:
  goals, fixed architecture decisions, ownership model, state model,
  reconciliation design, protocol strategy, test matrices, cleanup plan, and
  upstream monitoring strategy. **This is the source of truth for all new
  work.**
- [`ROADMAP_CHECKLIST_v2.md`](ROADMAP_CHECKLIST_v2.md) — the gated checklist
  tracking progress on each v2 roadmap item.
- [`docs/new plan/piCoreCDSP_v2_complete_roadmap(1).md`](docs/new%20plan/piCoreCDSP_v2_complete_roadmap%281%29.md)
  — the original uploaded plan the roadmap above formalizes.
- [`.github/copilot-instructions.md`](.github/copilot-instructions.md) —
  process rules enforced for any new contribution.

## Where the v1 implementation went

The previous dual-backend (`snd-aloop` + custom `ioplug`) Rust controller,
`picoredsp-ioplug` native ALSA module, installer script, benchmarks, and their
CI/upstream-tracking docs are **archived**, not deleted from history:

- Tag [`v1-final`](../../releases/tag/v1-final) — the exact `main` state
  immediately before this cutover.
- Branch [`v1-archive`](../../tree/v1-archive) — a long-lived branch pointing
  at the same commit, for easy `git checkout`/`git diff` without needing the
  tag.

Per the roadmap's Core Philosophy (§50, §53), v2 replaces v1 rather than
living beside it: there is no `legacy/`, `reference/`, or `old_controller/`
directory on `main`. Git history — the tag and branch above — is the sole
archive.

## Contributing

Read `piCoreCDSP_v2_Roadmap.md` and `ROADMAP_CHECKLIST_v2.md` in full before
proposing or implementing any change. Do not use the archived v1 planning
documents (only reachable via the `v1-final` tag / `v1-archive` branch) to
plan new work — they describe a superseded architecture and are not
maintained.

## References

- [HEnquist/camilladsp](https://github.com/HEnquist/camilladsp)
- [HEnquist/camilladsp-controller](https://github.com/HEnquist/camilladsp-controller)
- [HEnquist/camillagui-backend](https://github.com/HEnquist/camillagui-backend)
- [HEnquist/pycamilladsp](https://github.com/HEnquist/pycamilladsp)
- [JWahle/piCoreCDSP](https://github.com/JWahle/piCoreCDSP) — original ALSA cdsp-plugin based implementation
