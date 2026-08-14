#!/bin/bash

# Default behavior: show non-hidden folders only, skipping build artifacts
if [ "$1" == "-a" ]; then
    # Show non-hidden files and folders
    find . -not -path '*/.*' -not -path '*/target*' -not -path '*/node_modules*' -print | sed -e '1d' -e 's;[^/]*/;│  ;g' -e 's;│  \([^│]*\)$;├── \1;' -e 's;│  $;;'
else
    # Show non-hidden folders only
    find . -not -path '*/.*' -not -path '*/target*' -not -path '*/node_modules*' -type d -print | sed -e '1d' -e 's;[^/]*/;│  ;g' -e 's;│  \([^│]*\)$;├── \1;' -e 's;│  $;;'
fi

