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
   - [BlueALSA](https://github.com/Arkq/bluez-alsa) documentation and source code
   - [piCorePlayer](https://www.picoreplayer.org/) documentation
   - Any other library or API being used
3. **Search for existing solutions** in the codebase before writing new code.
4. Confirm your approach is consistent with patterns already established in the project.

## Testing Requirements

When implementing any new feature or fixing a bug, you **must** write detailed tests:

1. **Cover the happy path** — verify the feature works correctly under normal conditions.
2. **Cover edge cases** — test boundary conditions, empty inputs, maximum values, and unexpected states.
3. **Cover error paths** — verify that failures are handled and reported correctly.
4. **Use descriptive test names** — each test name should clearly describe what is being tested and what the expected outcome is.
5. **Keep tests isolated** — tests must not depend on each other or on external state; mock or stub external dependencies (ALSA, CamillaDSP websocket, BlueALSA) as needed.
6. **Run the full test suite** after adding new tests to confirm nothing is broken.

Tests are a required deliverable for every implementation task — a feature is not complete without them.

## Rust Formatting, Clippy, and Test Failures

After implementing new code, always:

1. Run Rust formatting checks and apply formatting fixes (`cargo fmt`).
2. Run Clippy checks and auto-correct issues when possible (`cargo clippy --fix`), then resolve any remaining warnings/errors.
3. If any tests fail, fix the issues and re-run formatting, Clippy, and tests until all checks pass.

## Verification Before Marking Done

A checklist item in `ROADMAP_CHECKLIST.md` must **not** be marked as done until:

1. The code implementing the feature is written and reviewed.
2. The implementation is verified to work correctly — test it or trace through the logic carefully.
3. Edge cases and error paths are handled.
4. The change does not break existing functionality.

Only after this verification should you update the checklist item to `[x]`.
