# Issue tracker: Local Markdown

Issues and specs for this repo live as markdown files in `.scratch/`.
Code is hosted on Gitee; issues deliberately stay in the repo.

## Conventions

- One feature per directory: `.scratch/<feature-slug>/`
- The spec is `.scratch/<feature-slug>/spec.md`
- Implementation issues are one file per ticket at
  `.scratch/<feature-slug>/issues/<NN>-<slug>.md`, numbered from `01`,
  never a single combined tickets file
- Triage state is recorded as a `Status:` line near the top of each issue file
- Comments and conversation history append to the bottom of the file under
  a `## Comments` heading

## When a skill says "publish to the issue tracker"

Create a new file under `.scratch/<feature-slug>/` (creating the directory
if needed).

## When a skill says "fetch the relevant ticket"

Read the file at the referenced path. The user will normally pass the path
or the issue number directly.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a file with one **child** file per ticket.

- **Map**: `.scratch/<effort>/map.md`
- **Child ticket**: `.scratch/<effort>/issues/NN-<slug>.md`, with a `Type:` line
  (`research`/`prototype`/`grilling`/`task`) and a `Status:` line
  (`claimed`/`resolved`)
- **Blocking**: a `Blocked by: NN, NN` line near the top
- **Frontier**: open, unblocked, unclaimed children; first by number wins
- **Claim**: set `Status: claimed` before any work
- **Resolve**: append `## Answer`, set `Status: resolved`, and add a gist +
  link to the map's Decisions-so-far
