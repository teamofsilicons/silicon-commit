# Commit UIArc refinement — 3 October 2026

The authenticated workspace uses quiet surfaces, compact account framing, pill actions, clearly selected tabs and consistent form focus. The verified session’s actor and organization remain visible in the header on mobile. At 390px the task table shows the title, status and open action together; assignment details remain available in the task detail and wider table.

Free UIArc Button, Input and Segmented Control styles were adapted to Solid and the existing app palette. `UIARC.md` records pinned source and MIT attribution. Native semantic buttons retain mutation and authorization behavior. Tabs now support arrow keys, Home and End with roving focus.

TypeScript, production build and all 26 frontend tests pass. Local browser checks exercised IAM fixture login, tab selection via arrow key, task creation dialog focus/cancel, account display and responsive layout. At 390×844, document and scroll width both equal 390. No task was submitted.

Evidence directory: `/Users/codanium/Documents/silicon/.codex-artifacts/iam5-app-updates-20261003/ui-audit/`

| View | Before | After |
| --- | --- | --- |
| Desktop | `03-commit-desktop-before.jpg` (1280×720) | `17-commit-desktop-after.jpg` (1440×1000) |
| Mobile | `06-commit-mobile-before.jpg` (390×844) | `09-commit-mobile-after.jpg` (390×844) |
| Create task | — | `18-commit-create-dialog-after.jpg` |

Mobile baseline was served from an artifact-only `git archive HEAD` of the functional IAM5 branch, using the same synthetic fixture; the initial incorrectly sized capture was replaced. All listed captures were opened and inspected. No production IAM or backend data was used. These checks do not establish full screen-reader coverage, every breakpoint or live deployment.
