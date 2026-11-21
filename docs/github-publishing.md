# Publishing a Linux Fork on GitHub

This guide connects the existing checkout to a GitHub fork. You run the commands
yourself; this document does not authorize publication or perform any GitHub action.

A **fork** is your GitHub copy linked to the original project. A **remote** is a
saved repository address. **Staging** selects changes for the next **commit**, a
local checkpoint. A **push** uploads committed checkpoints and their history to a
remote. Saving a file or making a commit alone does not upload it.

## 1. Get Written Permission Before Publishing
**STOP: do not publish modified source or binaries without prior written permission.**
The original project is <https://github.com/EinFabo/amberpc>, created by EinFabo.
Its `LICENSE` is custom all-rights-reserved, not Apache, requiring prior written permission
for modification and distribution. `package.json` and `Cargo.toml` say `UNLICENSED`.

GitHub's viewing/forking exception under its Terms of Service is not a general
license to modify or redistribute the project. A public fork is not evidence of
permission to publish Linux changes or packages. No permission is assumed here.

Request permission at <https://github.com/EinFabo/amberpc/issues>. If issues are
unavailable, use a contact method the creator provides. Suggested request:

> Hi EinFabo, I would like your written permission to maintain a Linux fork of
> amberpc. May I modify the project and publish the modified Linux source on
> GitHub, and distribute Linux packages/installers and binaries? May I use the
> existing Amber name and artwork, or should I rename and replace them? Please
> specify what downstream users may do with the source and packages, including
> modification and redistribution, and what credit/notices you require. I will
> credit you as the original creator and retain the existing license unless you
> explicitly authorize different terms. Please confirm the scope and conditions.

Keep the written grant and follow its scope. Source permission alone does not cover packages.
Keep `LICENSE` unchanged; do not replace it with Apache or invent downstream rights.
Until permission covers your planned work, stop here rather than proceeding below.

## 2. Inspect the Existing Checkout
Run all commands from your existing project directory, the one containing `README.md`.
Do not run `git init` or clone another copy over it.
These examples use Bash. Run commands one at a time and stop on errors.

```bash
git status --short
git branch -vv
git remote -v
git remote
```

At preparation time, this non-shallow repository has `main` tracking `origin/main`.
Its only remote is `origin`, pointing to `/srv/git/amber.git/`.
Upstream commit `64d67e308511cb922e1de957d7896b1cdd56e42d` is an ancestor of local
`main`, with eight local commits after it. A new fork may have a different tip.

Existing README and app/test edits must not be discarded, reset, or blindly merged over.
Review the current output rather than assuming it matches this snapshot. Confirm you are on `main`.

## 3. Create the GitHub Fork
1. Sign in to GitHub in your browser using your own account.
2. Open <https://github.com/EinFabo/amberpc> and click **Fork**.
3. Select your account as the owner and preferably name it `amber-linux`.
4. Creating only the default branch is sufficient for this guide.
5. Click **Create fork** and confirm the page says **forked from EinFabo/amberpc**.

Use GitHub's Fork action, not an unrelated new repository. The fork is public:
GitHub copies upstream content, not your local commits or uncommitted edits.
Creating the fork and configuring remotes do not upload your local changes; pushing does.

In later commands, replace `YOUR_GITHUB_USERNAME` with your actual username.
If you chose a different fork name, replace `amber-linux` too.

## 4. Install and Authenticate GitHub CLI
`gh` was not installed in the checked environment. Install it for your distribution,
choosing only the applicable option:

```bash
# Arch Linux:
sudo pacman -S github-cli

# Ubuntu:
sudo apt update
sudo apt install gh
```

Then authenticate as the same account that owns the fork:

```bash
gh auth login --hostname github.com --git-protocol https --web
gh auth setup-git
gh auth status
```

Follow the browser flow. Never share a token or browser one-time code in chat,
issues, screenshots, logs, or commits. Enter the code only on GitHub's device
authorization page reached through the official login flow.

## 5. Preserve the Local Remote and Add GitHub Remotes
First recheck `git remote -v` and `git remote`. Proceed only if `origin` is still
the local `/srv/git/amber.git/` remote and neither `local-backup` nor `upstream`
already exists. If names or URLs differ, stop and inspect; do not overwrite or delete
an existing remote blindly. Run the following once, replacing the username placeholder:

```bash
git remote rename origin local-backup
git remote add upstream https://github.com/EinFabo/amberpc.git
git remote add origin https://github.com/YOUR_GITHUB_USERNAME/amber-linux.git
git remote -v
git branch -vv
```

Verify both fetch and push URLs: `local-backup` must retain `/srv/git/amber.git/`,
`upstream` must point to `EinFabo/amberpc`, and `origin` must point to your fork.
The rename preserves the connection, not a new backup of uncommitted edits.
`main` now tracks `local-backup/main` until the later `push -u`.
Do not use a bare `git push` while that old tracking relationship remains.

## 6. Fetch and Check That a Safe Push Is Possible
```bash
git fetch origin
git fetch upstream
git merge-base --is-ancestor origin/main main
echo $?
```

Read the exit status immediately: `0` means `main` contains `origin/main` and can fast-forward it.
Any other value means stop, including a missing branch or command error. The check normally
prints nothing. Do not force-push, blindly merge, or reset to make it pass. Resolve divergence
separately, preserving local work. Fetching does not merge or publish files.

## 7. Review Identity, Security, and Changes
At preparation time, the eight local commits include two author/committer identities
with email addresses that are not GitHub `noreply` addresses. Publishing `main` exposes
those existing names and addresses. Decide whether that is acceptable before pushing;
do not rewrite the original creator's attribution as a privacy shortcut.

Check your identity and the authors/committers already recorded in history:

```bash
git config --get user.name
git config --get user.email
git log --format='%h %an <%ae> | %cn <%ce>' main
```

If you want a private commit email, copy your exact GitHub-provided `noreply`
address from GitHub **Settings > Emails**. These optional commands affect future
commits in this checkout only; replace both example values:

```bash
git config user.name "YOUR_NAME"
git config user.email "YOUR_EXACT_GITHUB_NOREPLY_ADDRESS"
```

Changing configuration does not rewrite previous commits or hide their emails. If history
contains private data, stop before pushing and resolve that separately. Audit the files and
history for credentials, recordings, transcripts, and personal paths. `.gitignore` does not
remove tracked files or historical data; these commands are not a security audit or safety guarantee.

Use the Git workflow below, not an upload of the entire project folder or a zip of it.
`git push` sends committed files and their history, not every local file or your
`.git/config`. A folder archive can accidentally include ignored credentials,
dependency directories, build outputs, or recordings that Git would not publish.

The preparation audit found no confirmed embedded credentials or large committed
artifacts across 115 commits and 918 unique historical file blobs. It used heuristic
checks, not a maintained secret scanner: `gitleaks`, `trufflehog`, `detect-secrets`,
and `git-secrets` were unavailable. This is not a guarantee that every secret was
found. Run a redacting secret scanner on the final history and staged files before
publication; do not paste unredacted scanner output into public issues.

Preparation items still to review:

- Add narrow `.gitignore` rules for local `.env` files, private credentials/keys,
  and repository-local copies of runtime data such as `secrets.bin`, `history.bin`,
  and `testing-tables.bin`. Preserve deliberate examples and public assets.
- Use dedicated ignored directories for recordings, transcripts, model downloads,
  and exports; do not ignore every JSON file or remove intentional audio test fixtures.
- Review developer names, machine paths, and environment details in old documentation
  and tests if privacy matters. Do not rewrite upstream history casually.
- Confirm the provenance of the voice-like decoder fixture
  `crates/core/tests/data/silk-voice-16k-60ms.ogg` if unknown. It is documented as a
  test signal, but the audit did not listen to or transcribe it.

Review `.github/workflows/` before pushing. Fork Actions may be disabled by
default; leave publishing workflows disabled until reviewed and authorized.
CI does not replace real Linux desktop tests for microphone, hotkeys, permissions,
and text insertion. Do not enable package uploading without distribution permission.
In particular, `.github/workflows/ci.yml` uploads Debian/RPM artifacts and
`.github/workflows/server-release.yml` publishes on matching server tags. Do not
push all existing tags as part of the first source upload.

```bash
git status --short
git diff --stat
git diff -- README.md
git diff --cached
```

Read `docs/github-publishing.md` in your editor: ordinary `git diff` omits untracked contents.
Review every outstanding app/test edit and deliberately include intended changes using explicit
paths. A documentation-only commit does not include those edits or make Linux ready to publish.
Inspect changes already staged by earlier work too; do not blindly reset them.

## 8. Stage Deliberately and Commit Locally
For example, to include the reviewed documentation changes:

```bash
git add -- README.md docs/github-publishing.md
git diff --cached --stat
git diff --cached
git status --short
```

Add other reviewed files by exact paths; avoid `git add .`. The staged diff must contain
only intended changes. Use a message describing those actual changes, for example:

```bash
git commit -m "Document Linux publishing workflow"
git status --short
```

A commit is local. Remaining uncommitted edits are not uploaded by a push.
Pushing will publish the existing local commits too, not just this new commit.

## 9. Preview and Push Explicitly to Your Fork
Recheck the destination, refresh the fork tip, and repeat the ancestry check.
Continue only when the exit status is `0`:

```bash
git remote -v
git fetch origin
git merge-base --is-ancestor origin/main main
echo $?
git push --dry-run origin main
```

Confirm the preview targets your GitHub fork, not upstream or the local remote.
A dry run does not update the remote branch. Only after the permission and review
gates above are satisfied, publish with:

```bash
git push -u origin main
```

This uploads committed source/history and sets `main` to track your fork's
`origin/main`. If rejected, stop and inspect; never add `--force` to bypass it.

## 10. Verify Source Publication; Treat Releases Separately
Pushing source does not itself create a GitHub Release or build an installer;
enabled workflows could publish artifacts, so review them first. The inherited
`tools/release.sh` and Windows updater still target the original repository; do not
use them for this Linux port. `.github/workflows/server-release.yml` publishes to
the repository where it runs when matching server tags are pushed, but the server
installer scripts still default to upstream downloads. None of these is a ready-made
Linux desktop release process. Linux packages, release assets, updater destinations,
and downstream rights need a separately reviewed workflow and permission covering
binary distribution, not just source.

On GitHub, verify the fork badge, Linux-focused README and original-creator credit,
default branch `main`, expected commit history, Actions status, and repository links.
Locally, `git branch -vv` should now show `main` tracking `origin/main`.
