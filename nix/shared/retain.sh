# Run with local control/results files; copy evidence after success or failure.
local_results="$1"
retained_results="$2"
shift 2
retain() {
  status=$?
  trap - EXIT
  if test -d "$local_results"; then
    cp -a --no-preserve=ownership -- "$local_results" "$retained_results" || exit 1
  fi
  exit "$status"
}
trap retain EXIT
trap 'exit 143' TERM
"$@"
