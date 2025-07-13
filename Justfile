# Generate demo
test-demo:
    vhs assets/demo.tape -o /tmp/pipec-demo.gif

# Regenerate demo and update README
update-demo:
    #!/usr/bin/env bash
    set -e

    # Render demo video
    vhs assets/demo.tape -o /tmp/pipec-demo.gif

    # Upload it
    URL=$(vhs publish --quiet /tmp/pipec-demo.gif)
    echo "Published: $URL"

    # Replace video link in README.md
    awk -v url="$URL" '/^!\[Demo\]/ { printf "![Demo](%s)\n", url; next } {print}' < README.md > README.md.tmp && mv README.md.tmp README.md
