#!/bin/sh
# fake-freebuff.sh - Offline test double for freebuff TUI
# POSIX-compliant; uses stty and dd for raw byte reading.

set -e

# Parse arguments to find --cwd
CWD=""
while [ $# -gt 0 ]; do
    case "$1" in
        --cwd)
            shift
            CWD="$1"
            ;;
    esac
    shift
done

if [ -z "$CWD" ]; then
    CWD="."
fi

# Get BLINK_MANICODE_DIR from env
MANICODE_DIR="${BLINK_MANICODE_DIR:-$HOME/.config/manicode}"

# Derive project name from cwd basename
PROJECT_NAME=$(basename "$CWD")

# Chat directory
CHATS_DIR="$MANICODE_DIR/projects/$PROJECT_NAME/chats"

# Read a single byte (raw mode)
read_byte() {
    dd bs=1 count=1 2>/dev/null || printf '\0'
}

# Print splash screen
print_splash() {
    cat <<'EOF'
████████████████████████████████████████████████████████
█                                                      █
█   F  R  E  E  B  U  F  F                            █
█                                                      █
████████████████████████████████████████████████████████

Start coding for free

┌──────────────────────────────────────────────────────┐
│  › GLM 5.3 Flash · Deep reasoning · Reasoning: max  │
│    Images · NEW                                      │
│  5 Freebucks/hr                                      │
│  FREE · 25/25 Freebucks daily · resets in 23h 59m   │
│  ↓  See all 4 models                                 │
│  ⌘ Copy invite link  Open Earn ↵                     │
└──────────────────────────────────────────────────────┘
EOF
}

# Print freebucks gate splash
print_gate_splash() {
    cat <<'EOF'
████████████████████████████████████████████████████████
█                                                      █
█   F  R  E  E  B  U  F  F                            █
█                                                      █
████████████████████████████████████████████████████████

Start coding for free

┌──────────────────────────────────────────────────────┐
│  › GLM 5.3 Flash · Deep reasoning · Reasoning: max  │
│    Images · NEW                                      │
│  Not enough Freebucks — 5 Freebucks/hr against 0    │
│  left. Enter opens plans.                            │
│  FREE · 0/25 Freebucks daily · resets in 2h 12m     │
│  ↓  See all 4 models                                 │
│  ⌘ Copy invite link  Open Earn ↵                     │
└──────────────────────────────────────────────────────┘
EOF
}

# Print already running dialog
print_already_running() {
    cat <<'EOF'
Freebuff is already running
Only one freebuff instance is allowed at a time.

  Take over    Exit
EOF
}

# Print idle screen
print_idle() {
    cat <<EOF
Freebuff will run commands on your behalf to help you build.
Directory $CWD
GLM 5.3 Flash · 59m left · 16.4K (2%)      ✕ End session
╭────╮
│  ▍Enter a coding task or / for commands  │
╰────╯
EOF
}

# Print status line when busy
print_status_busy() {
    printf 'thinking... 1s  ■ Esc\n'
    # Force flush in bash (macOS sh is bash in POSIX mode)
    exec 1>&1
}

# Print exit message
print_exit() {
    cat <<'EOF'
To continue this session later, run:
freebuff --continue <chat-id>
EOF
}

# Write log.jsonl line
write_log_line() {
    local msg="$1"
    local data="${2:-{}}"
    local line
    line=$(printf '{"level":30,"timestamp":"%s","pid":%d,"hostname":"test","msg":%s,"data":%s}\n' \
        "$(date -u +"%Y-%m-%dT%H:%M:%S.000Z")" \
        "$$" \
        "$(printf '%s' "$msg" | sed 's/"/\\"/g')" \
        "$data")
    printf '%s' "$line" >> "$CHAT_DIR/log.jsonl"
}

# Write chat-messages.json
write_chat_messages() {
    local prompt_text="$1"
    local mode="$2"
    cat > "$CHAT_DIR/chat-messages.json" <<EOF
[
  {
    "variant": "ai",
    "blocks": [
      {
        "type": "mode-divider",
        "mode": "$mode"
      }
    ],
    "isComplete": true
  },
  {
    "variant": "user",
    "content": "$prompt_text"
  },
  {
    "variant": "ai",
    "blocks": [
      {
        "type": "text",
        "textType": "reasoning",
        "content": "ok"
      },
      {
        "type": "tool",
        "toolCallId": "t1",
        "toolName": "run_terminal_command",
        "input": {
          "command": "echo hi",
          "process_type": "SYNC",
          "timeout_seconds": 30
        },
        "output": "hi\n",
        "agentId": "main-agent",
        "includeToolCall": true
      },
      {
        "type": "text",
        "textType": "text",
        "content": "PONG"
      }
    ],
    "isComplete": true,
    "completionTime": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
    "credits": 5,
    "metadata": {}
  }
]
EOF
}

# Setup terminal for raw input
stty raw -echo 2>/dev/null || true

# Cleanup on exit
cleanup() {
    stty -raw echo 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Handle special modes
case "${FAKE_FREEBUFF_MODE:-}" in
    gate)
        print_gate_splash
        # Block forever
        while true; do sleep 1; done
        ;;
    running)
        print_already_running
        # Block forever
        while true; do sleep 1; done
        ;;
esac

# Normal mode: print splash and wait for Enter
print_splash

# Wait for \r (Enter)
while true; do
    byte=$(read_byte)
    case "$byte" in
        $'\r') break ;;
    esac
done

# Clear screen and print idle screen
printf '\x1b[2J\x1b[H'
print_idle

# Read input loop
input_buffer=""
paste_buffer=""
in_bracketed_paste=0

while true; do
    byte=$(read_byte)
    case "$byte" in
        $'\x1b') # Escape sequence
            # Check for bracketed paste start ESC[200~
            next=$(read_byte)
            if [ "$next" = "[" ]; then
                third=$(read_byte)
                if [ "$third" = "2" ]; then
                    fourth=$(read_byte)
                    if [ "$fourth" = "0" ]; then
                        fifth=$(read_byte)
                        if [ "$fifth" = "0" ]; then
                            sixth=$(read_byte)
                            if [ "$sixth" = "~" ]; then
                                in_bracketed_paste=1
                                paste_buffer=""
                                continue
                            fi
                        fi
                    fi
                fi
            fi
            # Check for bracketed paste end ESC[201~
            if [ "$next" = "[" ]; then
                third=$(read_byte)
                if [ "$third" = "2" ]; then
                    fourth=$(read_byte)
                    if [ "$fourth" = "0" ]; then
                        fifth=$(read_byte)
                        if [ "$fifth" = "1" ]; then
                            sixth=$(read_byte)
                            if [ "$sixth" = "~" ]; then
                                in_bracketed_paste=0
                                # Echo the entire pasted text at once with newline to flush
                                if [ -n "$paste_buffer" ]; then
                                    printf '%s\n' "$paste_buffer"
                                    input_buffer="$paste_buffer"
                                    paste_buffer=""
                                fi
                                continue
                            fi
                        fi
                    fi
                fi
            fi
            # Handle ESC as cancel key during busy
            if [ "$in_bracketed_paste" = "0" ]; then
                # Echo ESC for visibility (it's the cancel key)
                printf '\x1b'
            fi
            ;;
        $'\r') # Enter
            if [ "$in_bracketed_paste" = "1" ]; then
                # Ignore Enter during bracketed paste
                continue
            fi
            # Check if input is /exit
            if [ "$input_buffer" = "/exit" ] || [ "$input_buffer" = "/quit" ]; then
                print_exit
                exit 0
            fi
            # If we have input, process the prompt
            if [ -n "$input_buffer" ]; then
                # Create chat directory with timestamp
                TIMESTAMP=$(date -u +"%Y-%m-%dT%H-%M-%S.000Z")
                CHAT_DIR="$CHATS_DIR/$TIMESTAMP"
                mkdir -p "$CHAT_DIR"

                # Write log.jsonl - TurnStarted
                write_log_line '[send-message] Sending message with sdk run config' '{}'

                # Write chat-messages.json
                write_chat_messages "$input_buffer" "LITE"

                # Print status line
                print_status_busy

                # Check for slow mode
                case "$input_buffer" in
                    *slow*)
                        # Wait up to 10 seconds for ESC (cancel)
                        waited=0
                        while [ $waited -lt 100 ]; do
                            # Use dd with timeout to check for ESC
                            byte=$(dd bs=1 count=1 2>/dev/null || printf '\0')
                            if [ "$byte" = $'\x1b' ]; then
                                # Cancel received - write interrupted log
                                write_log_line 'Agent execution failed' '{"error":{"name":"Error","message":"user-interrupt"}}'
                                write_log_line 'Main prompt finished' '{"outputType":"error"}'
                                break
                            fi
                            sleep 0.1
                            waited=$((waited + 1))
                        done
                        if [ $waited -ge 100 ]; then
                            # Timeout - normal completion
                            write_log_line 'Main prompt finished' '{"outputType":"lastMessage"}'
                        fi
                        ;;
                    *)
                        # Normal quick completion
                        sleep 2.0
                        write_log_line 'Main prompt finished' '{"outputType":"lastMessage"}'
                        ;;
                esac

                # Reprint idle screen
                print_idle
                input_buffer=""
            fi
            ;;
        $'\x7f'|$'\b') # Backspace
            if [ "$in_bracketed_paste" = "0" ] && [ -n "$input_buffer" ]; then
                # Remove last char from buffer (simplified - just clear for test)
                input_buffer=""
            fi
            ;;
        "")
            # EOF
            exit 0
            ;;
        *)
            # Printable character - accumulate
            if [ "$in_bracketed_paste" = "1" ]; then
                # During bracketed paste, accumulate in paste_buffer
                paste_buffer="${paste_buffer}${byte}"
            else
                # Normal input - echo and accumulate
                printf '%s' "$byte"
                input_buffer="${input_buffer}${byte}"
            fi
            ;;
    esac
done