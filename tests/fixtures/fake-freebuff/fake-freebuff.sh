#!/bin/sh
# fake-freebuff.sh - Offline test double for freebuff TUI
# POSIX-compliant; uses cooked-mode read for input.

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

if [ -z "$BLINK_MANICODE_DIR" ]; then
    echo "fake-freebuff: BLINK_MANICODE_DIR is required" >&2
    exit 64
fi

MANICODE_DIR="$BLINK_MANICODE_DIR"

# Derive project name from cwd basename
PROJECT_NAME=$(basename "$CWD")

# Chat directory
CHATS_DIR="$MANICODE_DIR/projects/$PROJECT_NAME/chats"

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

# Format a number with thousands separators (POSIX sh): 5001 → 5,001
format_number() {
    n=$1
    result=""
    while [ "$n" -gt 999 ] 2>/dev/null; do
        remainder=$(expr "$n" % 1000)
        n=$(expr "$n" / 1000)
        remainder=$(printf "%03d" "$remainder")
        result=",${remainder}${result}"
    done
    result="${n}${result}"
    printf "%s" "$result"
}

# Print chip row above the box and re-print box with placeholder
print_paste_chip_and_box() {
    local count=$1
    printf ' 📋 Pasted text (%s chars)\n' "$count"
    printf '╭────╮\n'
    printf '│  ▍Enter a coding task or / for commands  │\n'
    printf '╰────╯\n'
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
    local data="$2"
    [ -z "$data" ] && data="{}"
    local line
    line=$(    printf '{"level":30,"timestamp":"%s","pid":%d,"hostname":"test","msg":"%s","data":%s}\n' \
        "$(date -u +"%Y-%m-%dT%H:%M:%S.000Z")" \
        "$$" \
        "$(printf '%s' "$msg" | sed 's/"/\\"/g')" \
        "$data")
    printf '%s\n' "$line" >> "$CHAT_DIR/log.jsonl"
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

# Write unparsable chat-messages.json (for testing parse error handling)
write_unparsable_chat_messages() {
    cat > "$CHAT_DIR/chat-messages.json" <<'EOF'
[
  {
    "variant": "ai",
    "blocks": [
      {
        "type": "mode-divider",
        "mode": "LITE"
      }
    ],
    "isComplete": true
  },
  {
    "variant": "user",
    "content": "test"
  },
  {
    "variant": "ai",
    "blocks": [
      {
        "type": "text",
        "textType": "reasoning",
        "content": "ok"
      }
    ],
    "isComplete": true
  }
  // This trailing comma makes it invalid JSON
]
EOF
}

# Setup terminal for cooked input
stty sane 2>/dev/null || true

# Cleanup on exit
cleanup() {
    stty sane 2>/dev/null || true
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
    hang-splash)
        print_splash
        sleep 60
        ;;
    grandchild)
        # Behaves like normal mode, but right after printing the idle screen
        # for the first time it spawns an orphan grandchild
        # (`sh -c 'exec sleep 60' &`) that the launcher forwards no signals
        # to. The idle loop below (after `esac`) handles the rest.
        GRANDCHILD_SPAWNED=0
        ;;
    unparsable)
        print_splash
        read -r _
        idle_prompt() {
            printf '\033[2J\033[H'
            print_idle
            printf '\033[2A\033[4C'
            read -r line || return 1
            printf '\n\n'
            ESC=$(printf '\033')
            line=$(printf '%s' "$line" | sed -e "s/${ESC}\[200~//g" -e "s/${ESC}\[201~//g")
            if [ "$line" = "/exit" ]; then
                print_exit
                exit 0
            fi
            TIMESTAMP=$(date -u +"%Y-%m-%dT%H-%M-%S.000Z")
            CHAT_DIR="$CHATS_DIR/$TIMESTAMP"
            mkdir -p "$CHAT_DIR"
            write_log_line '[send-message] Sending message with sdk run config' '{}'
            write_unparsable_chat_messages
            print_status_busy
            sleep 0.5
            write_log_line 'Main prompt finished' '{"outputType":"lastMessage"}'
            print_idle
        }
        while true; do
            idle_prompt || break
        done
        ;;
    late-transcript)
        print_splash
        read -r _
        idle_prompt() {
            printf '\033[2J\033[H'
            print_idle
            printf '\033[2A\033[4C'
            read -r line || return 1
            printf '\n\n'
            ESC=$(printf '\033')
            line=$(printf '%s' "$line" | sed -e "s/${ESC}\[200~//g" -e "s/${ESC}\[201~//g")
            if [ "$line" = "/exit" ]; then
                print_exit
                exit 0
            fi
            TIMESTAMP=$(date -u +"%Y-%m-%dT%H-%M-%S.000Z")
            CHAT_DIR="$CHATS_DIR/$TIMESTAMP"
            mkdir -p "$CHAT_DIR"
            write_log_line '[send-message] Sending message with sdk run config' '{}'
            print_status_busy
            sleep 0.5
            # Write log line FIRST, then sleep, then write chat-messages.json
            write_log_line 'Main prompt finished' '{"outputType":"lastMessage"}'
            sleep 0.4
            write_chat_messages "$line" "LITE"
            print_idle
        }
        while true; do
            idle_prompt || break
        done
        ;;
esac

# Normal mode: print splash and wait for Enter
printf '\033[2J\033[H'
print_splash
read -r _

# Idle prompt: clear screen, print idle, position cursor in input box, read line
idle_prompt() {
    printf '\033[2J\033[H'
    print_idle
    if [ "$FAKE_FREEBUFF_MODE" = "grandchild" ] && [ "$GRANDCHILD_SPAWNED" -eq 0 ]; then
        # Spawn an orphan grandchild in the same session/process group.
        # The launcher forwards no signals to it, so only a group kill
        # reaches it.
        sh -c 'exec sleep 60' &
        GRANDCHILD_SPAWNED=1
    fi
    printf '\033[2A\033[4C'
    read -r line || return 1
    printf '\n\n'
    ESC=$(printf '\033')
    line=$(printf '%s' "$line" | sed -e "s/${ESC}\[200~//g" -e "s/${ESC}\[201~//g")
    line_len=${#line}
    if [ "$line_len" -gt 1000 ]; then
        formatted=$(format_number "$line_len")
        print_paste_chip_and_box "$formatted"
    fi
    if [ "$line" = "/exit" ]; then
        print_exit
        exit 0
    fi
    TIMESTAMP=$(date -u +"%Y-%m-%dT%H-%M-%S.000Z")
    CHAT_DIR="$CHATS_DIR/$TIMESTAMP"
    mkdir -p "$CHAT_DIR"
    write_log_line '[send-message] Sending message with sdk run config' '{}'
    write_chat_messages "$line" "LITE"
    print_status_busy
    case "$line" in
        *slow*)
            stty raw -echo
            waited=0
            while [ $waited -lt 100 ]; do
                b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' ')
                if [ "$b" = "1b" ]; then
                    write_log_line 'Agent execution failed' '{"error":{"name":"Error","message":"user-interrupt"}}'
                    write_log_line 'Main prompt finished' '{"outputType":"error"}'
                    break
                fi
                sleep 0.1
                waited=$((waited + 1))
            done
            stty sane
            if [ $waited -ge 100 ]; then
                write_log_line 'Main prompt finished' '{"outputType":"lastMessage"}'
            fi
            ;;
        *)
            sleep 0.5
            write_log_line 'Main prompt finished' '{"outputType":"lastMessage"}'
            ;;
    esac
    print_idle
}

while true; do
    idle_prompt || break
done