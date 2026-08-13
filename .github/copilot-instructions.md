# GitHub Copilot Instructions

## Roadmap Files Are the Source of Truth

The following files are the authoritative reference for all work in this repository:

- `piCoreDSP_Dual_Backend_Roadmap.md` — the full roadmap with goals, architecture decisions, and implementation details
- `ROADMAP_CHECKLIST.md` — the checklist tracking progress on each roadmap item

**Before writing any code, always:**
1. Read `piCoreDSP_Dual_Backend_Roadmap.md` and `ROADMAP_CHECKLIST.md` to understand the current state and goals.
2. Identify which checklist item the work relates to.
3. Ensure your implementation aligns with the roadmap intent.

## Research Before Implementation

Before implementing any feature or making a significant change:

1. **Read the roadmap files** for context, requirements, and architecture notes.
2. **Research upstream references** — check the official documentation and source code for all relevant projects:
   - [CamillaDSP](https://github.com/HEnquist/camilladsp) and its [pycamilladsp](https://github.com/HEnquist/pycamilladsp) Python client
   - [ALSA](https://www.alsa-project.org/wiki/Main_Page) documentation
   - [piCorePlayer](https://www.picoreplayer.org/) documentation
   - Any other library or API being used
3. **Search for existing solutions** in the codebase before writing new code.
4. Confirm your approach is consistent with patterns already established in the project.

## Verification Before Marking Done

A checklist item in `ROADMAP_CHECKLIST.md` must **not** be marked as done until:

1. The code implementing the feature is written and reviewed.
2. The implementation is verified to work correctly — test it or trace through the logic carefully.
3. Edge cases and error paths are handled.
4. The change does not break existing functionality.

Only after this verification should you update the checklist item to `[x]`.
