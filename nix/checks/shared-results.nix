{ pkgs }:
let
  retain = import ../shared/retain.nix { inherit pkgs; };
in
pkgs.runCommand "cas-shared-results-check" { } ''
  mkdir -p retained
  for status in 0 17; do
    actual=0
    ${retain}/bin/cas-retain-shared-results "local-$status" "retained/run-$status" \
      ${pkgs.bash}/bin/bash -c 'mkdir "$1"; printf evidence > "$1/report"; chmod +x "$1/report"; exit "$2"' \
      -- "local-$status" "$status" || actual=$?
    test "$actual" = "$status"
    cmp "local-$status/report" "retained/run-$status/report"
    test -x "retained/run-$status/report"
  done
  touch "$out"
''
