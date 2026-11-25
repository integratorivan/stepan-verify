---
name: describe-pr
description: Create or update the pull request for the current branch with a short reviewer-oriented description, then watch CI until it is green. Only use when the user explicitly invokes this skill by name.
disable-model-invocation: true
---

<!-- Adapted from humanlayer/humanlayer describe-pr (Apache-2.0, https://github.com/humanlayer/humanlayer). Shipped and kept current by Stepan; local edits are overwritten. -->

# Describe a pull request and get it green

Create or update the pull request for the current branch with a concise description that helps a reviewer understand why the change exists and the shape of the implementation. Then stay with the PR until CI passes.

## 1. Find or create the PR

- If `gh` is missing or signed out (`gh auth status`), stop and tell the user. Do not work around it.
- Look for a PR on the current branch: `gh pr view --json url,number,title,state,baseRefName,headRefName 2>/dev/null`.
- If there is none, inspect `git status --short --branch` and the branch's commits. Commit the work that belongs to this change, push the branch with an upstream, and create the PR (`gh pr create`). Never force-push, never commit secrets or `.env*` files, never push to the default branch directly; if you are on the default branch, create a branch first.
- Ask the user to pick a PR only when the current branch has no relevant work and there is no safe PR to create.

## 2. Gather only what is needed to explain the change

- Read the complete PR diff (`gh pr diff {number}`) and enough surrounding code to understand behaviour and ownership.
- If a Stepan task is linked (`$STEPAN_VERIFY_TASK` is set), read it with `stepan_verify task show "$STEPAN_VERIFY_TASK"` for the original request.
- Read the project's plan or decision documents if the session used them (Stepan: `$STEPAN_VERIFY_DOCS/<project>/`).
- Language: write the PR in the language the repository's recent PRs use (`gh pr list --state merged --limit 3 --json title,body`); with no history, use the language the user talks to you in.

## 3. Write the description

Use exactly this structure, and nothing beyond it:

```markdown
## Why the change

{Exactly one sentence: the problem this PR solves and what becomes possible after it ships.}

## Special things to note

- {1–3 bullets: reviewer warnings, migrations, compatibility constraints, deliberate omissions, surprising decisions. Write "- None." when there are none.}

## Change outline

{The smallest set of structural views that explains the implementation, each with a one-line lead-in.}
```

For **Change outline**, follow the conventions of the `show-me` skill (read its SKILL.md if it is installed): compact structural views, not prose and not a file-by-file changelog. Include only views that help explain this PR:

- SQL tables and endpoint request/response contracts; pseudocode for changed business logic.
- A shallow file tree with changed responsibilities.
- Component trees with important hooks, state and package boundaries.
- Call trees, control flow or data flow.

Prefer ` ```diff ` blocks for changes to an existing shape (first column `+`, `-` or space). Show the complete target shape in a plain block when most of it is new or a diff would hide order and ownership. Order the views so the story is easiest to follow. Do not add walkthroughs, verification checklists, changelog text or plan-deviation sections.

Write as one person talking to another: plain, coherent, concise.

## 4. Publish it

- Save the body to `/tmp/describe-pr/pr-{number}.md` and run `gh pr edit {number} --body-file /tmp/describe-pr/pr-{number}.md`. Confirm it succeeded.
- If `$STEPAN_VERIFY_TASK` is set, record the PR on the task: `stepan_verify task log "PR: {pr_url}"`.
- Send the user a short report — PR link, two or three sentences on what it does, the list of changed files — and say you are now waiting for CI. Then keep working in the same turn: step 5 follows immediately.

## 5. Watch CI until it passes

- Wait 5 minutes so checks can start: `sleep 300` (Bash timeout above 300000 ms).
- Check for merge conflicts: `gh pr view {number} --json mergeable,mergeStateStatus`. If `mergeable` is `UNKNOWN`, wait 30 seconds and ask again.
- If `mergeable` is `CONFLICTING`:
  - Do not merge, rebase, or edit files to resolve the conflicts.
  - Fetch the base branch (`git fetch {remote} {baseRefName}`) and list the conflicting files: `git merge-tree --write-tree --name-only HEAD {remote}/{baseRefName}`.
  - For each conflicting file, compare both sides (`git log --oneline HEAD...{remote}/{baseRefName} -- {file}` and the diffs) to learn what each side changed.
  - Offer the user a merge plan: which base commits conflict, how to resolve each file (keep ours, keep theirs, or combine — and why), and which checks to run afterwards.
  - Stop and wait for approval. Do not watch CI while the PR has conflicts.
- Check status with `gh pr checks {number}`. If there are no checks yet, wait 2 more minutes and look again, at most 3 times. If there are still none, tell the user and stop.
- While checks are pending: `gh pr checks {number} --watch --fail-fast` (Bash timeout 600000 ms). If it times out, run it again.
- When a check fails:
  - Read the failed logs: `gh run view {run-id} --log-failed`, and find the cause.
  - If this PR caused it: fix it, run the matching check locally, commit, push, and go back to watching.
  - If it is flaky or unrelated: rerun once with `gh run rerun {run-id} --failed`. If it fails again, stop and tell the user what failed and why.
  - After 3 fix attempts on the same check, stop and ask the user.
- When all checks pass, send one short final message saying so, with the PR link. If `$STEPAN_VERIFY_TASK` is set, also run `stepan_verify task log "CI зелёный: {pr_url}"`.
