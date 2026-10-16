#!/usr/bin/env bash
# Puts or drops one context folder on the screen gallery branch
# (.github/workflows/ui-gallery.yml).
#
#   ui-gallery-branch.sh put  <folder> <pictures-dir> <commit message>
#   ui-gallery-branch.sh drop <folder> ""             <commit message>
#
# The branch ($GALLERY_BRANCH, default ui-gallery) is one commit: a README and
# one folder per context (pr-<number>, master, ref-<name>). Each call reads
# the tip, replaces or removes the folder, commits the whole tree as a new
# parentless commit and pushes it with --force-with-lease on the tip it read. A
# publish that raced another one fails the lease and starts over from the new
# tip, so neither drops the other's folder, and old pictures never pile up in
# the repository's history. Prints the commit and, under Actions, writes
# sha=<commit> to $GITHUB_OUTPUT.
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 put|drop <folder> <pictures-dir> <message>" >&2
  exit 2
fi
mode="$1"
folder="$2"
pictures="$3"
message="$4"
branch="${GALLERY_BRANCH:-ui-gallery}"

case "$mode" in put | drop) ;; *) echo "mode must be put or drop, not '$mode'" >&2; exit 2 ;; esac
case "$folder" in '' | *[!A-Za-z0-9._-]* | .*) echo "bad context folder '$folder'" >&2; exit 2 ;; esac
if [ "$mode" = put ] && ! compgen -G "$pictures/*.png" >/dev/null; then
  echo "no pictures in '$pictures'" >&2
  exit 1
fi

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"

tree="$(mktemp -d)"
cleanup() {
  git worktree remove --force "$tree" >/dev/null 2>&1 || true
  rm -rf "$tree"
}
trap cleanup EXIT

readme() {
  cat <<'EOF'
# ui-gallery

이 브랜치는 `.github/workflows/ui-gallery.yml`이 씁니다. 손으로 고치지 마십시오.

폴더 하나가 화면 갤러리 하나입니다(`pr-<번호>`, `master`, `ref-<이름>`). 그림은
`<장면>.<언어>.<팔레트>.png`이고 장면 표는 master의
`apps/spatial-softreceiver/ui/e2e/gallery.json`입니다. 게시할 때마다 커밋 하나로
바꿔 쓰므로 이 브랜치에는 내력이 없습니다. 갤러리는 `ui-gallery` 라벨이 붙은
이슈에서 봅니다.
EOF
}

for attempt in 1 2 3 4 5; do
  cleanup
  tree="$(mktemp -d)"
  rmdir "$tree"

  status=0
  git ls-remote --exit-code origin "refs/heads/$branch" >/dev/null || status=$?
  if [ "$status" -eq 0 ]; then
    git fetch --quiet --no-tags --depth=1 origin "+refs/heads/$branch:refs/remotes/origin/$branch"
    base="$(git rev-parse "refs/remotes/origin/$branch")"
    git worktree add --quiet --detach "$tree" "$base"
  elif [ "$status" -eq 2 ]; then
    base=""
    if [ "$mode" = drop ]; then
      echo "no $branch branch; nothing to drop"
      exit 0
    fi
    git worktree add --quiet --detach "$tree" HEAD
    git -C "$tree" rm -rfq .
  else
    echo "git ls-remote failed ($status)" >&2
    exit 1
  fi

  if [ "$mode" = drop ] && [ ! -d "$tree/$folder" ]; then
    echo "$folder is not on $branch; nothing to drop"
    exit 0
  fi
  rm -rf "${tree:?}/$folder"
  if [ "$mode" = put ]; then
    mkdir -p "$tree/$folder"
    cp "$pictures"/*.png "$tree/$folder/"
  fi
  readme > "$tree/README.md"
  git -C "$tree" add -A
  # A commit without a parent: the branch never keeps the pictures it drops.
  sha="$(git -C "$tree" commit-tree "$(git -C "$tree" write-tree)" -m "$message")"

  if git push --quiet --force-with-lease="refs/heads/$branch:$base" origin "$sha:refs/heads/$branch"; then
    echo "$branch is now $sha ($mode $folder)"
    if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "sha=$sha" >> "$GITHUB_OUTPUT"; fi
    exit 0
  fi
  echo "push to $branch was refused (attempt $attempt); reading the tip again" >&2
  sleep $((attempt * 3 + RANDOM % 4))
done
echo "could not update $branch after 5 attempts" >&2
exit 1
