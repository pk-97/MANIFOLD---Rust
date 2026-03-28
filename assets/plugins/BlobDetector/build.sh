#!/bin/bash
# Build BlobDetector native plugin as a macOS bundle for Unity
# Embeds OpenCV dylibs so the bundle is self-contained (no Homebrew dependency at runtime)
# Requires: brew install opencv
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUNDLE_NAME="BlobDetector"
BUNDLE_DIR="$SCRIPT_DIR/../${BUNDLE_NAME}.bundle"
CONTENTS_DIR="$BUNDLE_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
LIBS_DIR="$CONTENTS_DIR/Frameworks"

echo "Building $BUNDLE_NAME..."

# Get OpenCV paths from Homebrew
OPENCV_PREFIX=$(brew --prefix opencv)
OPENCV_INCLUDE="$OPENCV_PREFIX/include/opencv4"
OPENCV_LIB="$OPENCV_PREFIX/lib"

if [ ! -d "$OPENCV_INCLUDE" ]; then
    echo "Error: OpenCV not found. Install with: brew install opencv"
    exit 1
fi

# Clean previous build
rm -rf "$BUNDLE_DIR"
mkdir -p "$MACOS_DIR" "$LIBS_DIR"

# Compile as dynamic library (ARM64 for Apple Silicon)
# Use @rpath so we can point it at the embedded Frameworks dir
clang++ -dynamiclib \
    -arch arm64 \
    -mmacosx-version-min=13.0 \
    -std=c++17 \
    -O2 \
    -I"$OPENCV_INCLUDE" \
    -L"$OPENCV_LIB" \
    -lopencv_core -lopencv_imgproc \
    -Wl,-rpath,@loader_path/../Frameworks \
    -install_name "@rpath/$BUNDLE_NAME" \
    -o "$MACOS_DIR/$BUNDLE_NAME" \
    "$SCRIPT_DIR/BlobDetectorPlugin.cpp"

# --- Embed OpenCV dylibs ---

# Copies a dylib into Frameworks/ and rewrites the reference in the dependent binary.
# Also fixes the dylib's own install name to @rpath-relative.
embed_dylib() {
    local binary="$1"    # binary that depends on the dylib
    local orig_path="$2" # absolute path as shown by otool

    local dylib_name
    dylib_name=$(basename "$orig_path")
    local dest="$LIBS_DIR/$dylib_name"

    # Only copy once (multiple binaries may share a dep)
    if [ ! -f "$dest" ]; then
        # Resolve symlinks to get the real file
        local real_path
        real_path=$(realpath "$orig_path")
        cp "$real_path" "$dest"
        chmod 644 "$dest"

        # Set the dylib's own id to @rpath-relative
        install_name_tool -id "@rpath/$dylib_name" "$dest"

        # Recursively embed any non-system deps this dylib itself has
        embed_deps "$dest"
    fi

    # Rewrite the reference in the dependent binary
    install_name_tool -change "$orig_path" "@rpath/$dylib_name" "$binary"
}

# Finds all non-system dylib dependencies and embeds them.
embed_deps() {
    local binary="$1"
    # otool -L output format: "\tpath (compatibility version ...)"
    # Strip leading whitespace and the parenthesized version suffix to get the path.
    otool -L "$binary" | tail -n +2 | while read -r line; do
        local dep
        dep=$(echo "$line" | sed 's/^[[:space:]]*//' | sed 's/ (compatibility.*//' | sed 's/ (current.*//')
        # Skip system/OS frameworks, already-rewritten refs, and empty lines
        case "$dep" in
            /usr/lib/*|/System/*|@rpath/*|@loader_path/*|"") continue ;;
        esac
        embed_dylib "$binary" "$dep"
    done
}

echo "Embedding OpenCV dylibs..."
embed_deps "$MACOS_DIR/$BUNDLE_NAME"

# Create Info.plist
cat > "$CONTENTS_DIR/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>${BUNDLE_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>com.manifold.BlobDetector</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>${BUNDLE_NAME}</string>
    <key>CFBundlePackageType</key>
    <string>BNDL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>CFBundleSupportedPlatforms</key>
    <array>
        <string>MacOSX</string>
    </array>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
</dict>
</plist>
PLIST

# Ad-hoc code sign the entire bundle (including embedded dylibs)
codesign --force --deep --sign - "$BUNDLE_DIR"

echo "Built: $BUNDLE_DIR"
echo "Embedded libs:"
ls -la "$LIBS_DIR/"
file "$MACOS_DIR/$BUNDLE_NAME"
