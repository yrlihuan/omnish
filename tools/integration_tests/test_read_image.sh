#!/usr/bin/env bash
#
# test_read_image.sh - Integration test for the read_image tool (#690).
#
# Needs a daemon and client built with read_image, and a chat backend that
# accepts images (e.g. deepseek-flash on the Anthropic-compatible endpoint).
# Test images are generated with python3.
#
# Test cases:
#   1. The model views a small PNG through read_image and names its color
#   2. A large PNG is downscaled to 1568px on the long edge and stays PNG
#   3. /resume shows the image description, never the base64 payload

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

show_usage() {
    cat <<EOF
Test cases:
  1. Model views a small PNG via read_image and names its color
  2. Large PNG downscaled to 1568px and kept as PNG
  3. /resume shows the image description, not the base64 payload
EOF
}

test_init "read-image" "$@"

IMG_DIR="$(mktemp -d /tmp/omnish-test-images.XXXXXX)"
trap 'rm -rf "$IMG_DIR"; cleanup' EXIT

# make_png <path> <width> <height> <r> <g> <b>  - solid-color RGB PNG
make_png() {
    python3 - "$@" <<'PY'
import struct, sys, zlib
path, w, h, r, g, b = sys.argv[1], *map(int, sys.argv[2:])
row = b"\x00" + bytes([r, g, b]) * w
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(row * h))
       + chunk(b"IEND", b""))
open(path, "wb").write(png)
PY
}

# assert_no_payload <content> - the base64 image must never reach the screen
assert_no_payload() {
    if echo "$1" | grep -qE 'omnish-image|iVBORw0KGgo'; then
        assert_fail "Image payload (marker/base64) leaked onto the screen"
        return 1
    fi
    return 0
}

# ── Test 1: model sees the image ─────────────────────────────────────────
# A neutral file name keeps "red" out of the prompt and the tool status, so
# the answer can only come from the pixels.
test_1() {
    echo -e "\n${YELLOW}=== Test 1: read_image lets the model see a PNG ===${NC}"
    local img="$IMG_DIR/sample.png"
    make_png "$img" 200 200 230 20 20

    restart_client
    wait_for_client
    enter_chat
    send_keys "Use the read_image tool to view $img. Reply with only the main color of the image as one lowercase English word." 0.3
    send_enter 1

    if ! wait_for_chat_response; then
        show_capture "After read_image" "$(capture_pane -60)" 40
        assert_fail "No chat response after read_image"
        return 1
    fi
    local content
    content=$(capture_pane -60)
    show_capture "read_image turn" "$content" 30

    if ! echo "$content" | grep -q 'ReadImage('; then
        assert_fail "No '● ReadImage(' tool header: the model did not call read_image"
        return 1
    fi
    if ! echo "$content" | grep -qF "Image: $img (200x200, png, "; then
        assert_fail "Tool status does not show the image description"
        return 1
    fi
    assert_no_payload "$content" || return 1
    # The answer is the last "●" line that is not the tool header.
    local answer
    answer=$(echo "$content" | grep -E '^\s*●' | grep -v 'ReadImage(' | tail -1)
    if echo "$answer" | grep -qi 'red'; then
        assert_pass "Model saw the image (answer: ${answer})"
    else
        assert_fail "Model did not recognize the color (answer: ${answer:-<none>})"
        return 1
    fi
}

# ── Test 2: large image is downscaled ────────────────────────────────────
test_2() {
    echo -e "\n${YELLOW}=== Test 2: large PNG downscaled to 1568px, stays PNG ===${NC}"
    local img="$IMG_DIR/large.png"
    make_png "$img" 3000 2000 20 20 230

    restart_client
    wait_for_client
    enter_chat
    send_keys "Use the read_image tool to view $img, then reply with only the word OK." 0.3
    send_enter 1

    if ! wait_for_chat_response; then
        show_capture "After read_image" "$(capture_pane -60)" 40
        assert_fail "No chat response after read_image"
        return 1
    fi
    local content
    content=$(capture_pane -60)
    show_capture "read_image turn" "$content" 30
    assert_no_payload "$content" || return 1
    if echo "$content" | grep -qF "Image: $img (1568x1045, resized from 3000x2000, png, "; then
        assert_pass "Downscaled to 1568x1045 and kept as PNG"
    else
        assert_fail "Expected 'Image: $img (1568x1045, resized from 3000x2000, png, ...' in the tool status"
        return 1
    fi
}

# ── Test 3: /resume shows the description, not the payload ───────────────
# Resumes the most recent thread (test 2's when run in sequence, test 1's
# otherwise); its history is rebuilt from the stored image tool_result.
test_3() {
    echo -e "\n${YELLOW}=== Test 3: /resume shows the image description ===${NC}"
    local img="$IMG_DIR/resume.png"
    make_png "$img" 64 64 20 230 20

    restart_client
    wait_for_client
    enter_chat
    send_keys "Use the read_image tool to view $img, then reply with only the word OK." 0.3
    send_enter 1
    if ! wait_for_chat_response; then
        assert_fail "No chat response after read_image"
        return 1
    fi
    send_special Escape 1

    enter_chat
    send_keys "/resume 1" 0.3
    send_enter 3
    local content
    content=$(capture_pane -100)
    show_capture "/resume 1" "$content" 30
    assert_no_payload "$content" || return 1
    if echo "$content" | grep -qF "Image: $img (64x64, png, "; then
        assert_pass "Resumed history shows the image description"
    else
        assert_fail "Resumed history does not show the image description"
        return 1
    fi
}

run_tests 3
