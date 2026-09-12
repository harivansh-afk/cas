# Copy the JSON build record into the runner's output for result provenance.

mkdir -p "$out/share/cas"
cp "$CAS_BUILD_INFO" "$out/share/cas/build.json"
