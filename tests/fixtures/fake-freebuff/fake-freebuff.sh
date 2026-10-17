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

# One chat directory per session, reused across turns (like real freebuff,
# which appends subsequent prompts to the same chat). Created lazily on the
# first prompt.
CHAT_DIR=""

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
████████████████████████████████████████
█                                                      █
█   F  R  E  E  B  U  F  F                            █
█                                                      █
████████████████████████████████████████

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
████████████████████████████████████████
█                                                      █
█   F  R  E  E  B  U  F  F                            █
█                                                      █
████████████████████████████████████████

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

# Print the freebuff 0.2.x HOURS gate (bills a session in hours; a turn is
# refused). Mirrors the real interstitial captured on 2026-10-16 (v0.2.29).
print_hours_gate() {
    cat <<'EOF'
████████████████████████████████████████
█                                                      █
█   F  R  E  E  B  U  F  F                            █
█                                                      █
████████████████████████████████████████

┌──────────────────────────────────────────────────────┐
│ Not enough hours for MiMo Fast: an hour on it takes 2 hours.
│ Choose another model with /model or visit https://freebuff.com/plans. │
│ Enter: retry  Esc: back to draft                     │
└──────────────────────────────────────────────────────┘
 MiMo Fast · /model to change
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

# Idle screen whose input box is preceded by freebuff's pasted-text chip.
print_idle_with_chip() {
    printf '  Freebuff will run commands on your behalf to help you build.\n\n'
    printf '  Directory %s\n\n' "$CWD"
    printf 'GLM 5.3 Flash · 59m left · 16.4K (2%%)      ✕ End session\n'
    print_paste_chip_and_box "1,500"
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
    hours-gate)
        print_hours_gate
        # Block forever: the real interstitial waits for Enter/Esc, which
        # bufflink must NOT send (AGENTS.md: never auto-dismiss an interstitial).
        while true; do sleep 1; done
        ;;
    running)
        print_already_running
        # Block forever
        while true; do sleep 1; done
        ;;
    no-auth)
        # freebuff with no usable auth token: within ~50 ms it writes the
        # signature to its own log and then goes silent forever. No login
        # screen ever appears, so the screen classifier only ever sees Booting.
        TIMESTAMP=$(date -u +"%Y-%m-%dT%H-%M-%S.000Z")
        CHAT_DIR="$CHATS_DIR/$TIMESTAMP"
        mkdir -p "$CHAT_DIR"
        write_log_line '[ads] No auth token available' '{}'
        write_log_line 'No authentication token found. Please run the login flow or set CODEBUFF_API_KEY.' '{}'
        write_log_line '[chat-runtime] Freebuff session over; holding queued messages until rejoin' '{}'
        write_log_line '[freebuff-session] No auth token; skipping free-session admission' '{}'
        # Block forever, like the real binary with no token.
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
    chip)
        # Normal flow; idle_prompt shows the chip and reads without echo.
        ;;
    prompt-gate|prompt-stall)
        # Normal startup flow reaching Idle; the gate/stall only appears once a
        # prompt is submitted. Handled in idle_prompt below. Opt-in so no
        # existing test changes behaviour.
        ;;
    slow-busy)
        # Normal flow; idle_prompt shows the placeholder (paste consumed) for
        # 6 s before going busy.
        ;;
    slow-legacy-interrupt)
        # Normal flow; the cancel path emits the LEGACY `user-interrupt` form
        # instead of the real cancellation line (see the *slow* arm below).
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
    list)
        # 0.2.29 flow, modelled from the live probe (2026-10-17): freebuff boots
        # STRAIGHT to the idle input box with a model already selected — there is
        # NO startup picker — and the model picker is opened by SUBMITTING the
        # `/model` command. The fake mirrors that, including the slash-
        # autocomplete popup that SWALLOWS the first Enter (documented in
        # docs/research/freebuff-tui.md): idle -> `/model` (first Enter eaten) ->
        # picker, driven with Up/Down and Enter.
        #
        # Enter on a model row is an Enter-guard violation, so the fake exits 1
        # to prove blink never sends Enter on a model row.
        #
        # POSIX sh has no arrays and the shebang is #!/bin/sh (dash on Ubuntu):
        # keep the ten model display names, in order, as a newline-delimited
        # list and look each model's cost up by name.
        MODEL_NAMES='GLM 5.3 Flash
MiMo 2.6 Pro
MiMo 2.6 Flash
GPT-6 Luna
GPT-6.1 Sol
DeepSeek V4.1 Flash
Solar Pro 4
Solar Mini 4
Muse Spark 1.3
Gemini 3.8 Flash'
        N=$(printf '%s\n' "$MODEL_NAMES" | wc -l | tr -d ' ')
        model_cost() {
            case "$1" in
                'GLM 5.3 Flash') printf '5' ;;
                'MiMo 2.6 Pro') printf '10' ;;
                'MiMo 2.6 Flash') printf '3' ;;
                'GPT-6 Luna') printf '8' ;;
                'GPT-6.1 Sol') printf '12' ;;
                'DeepSeek V4.1 Flash') printf '15' ;;
                'Solar Pro 4') printf '10' ;;
                'Solar Mini 4') printf '4' ;;
                'Muse Spark 1.3') printf '6' ;;
                'Gemini 3.8 Flash') printf '7' ;;
            esac
        }
        ESC=$(printf '\033')
        CR=$(printf '\r')
        line=""
        mode=idle
        submit_seen=0      # idle: has the first (swallowed) Enter been sent?
        menu=0             # idle: is the slash-autocomplete popup shown?
        expanded=0
        focus=0            # 0..N-1 model index; N = the toggle row (expanded only)
        focus_toggle=0     # collapsed: 1 means focus on the See-all toggle
        scroll=0
        V=$(( (${LINES:-40} - 8) / 2 ))
        [ "$V" -lt 1 ] && V=1
        [ "$V" -gt "$N" ] && V=$N

        stty raw -echo 2>/dev/null || true

        draw_idle() {
            printf '\033[2J\033[H'
            printf 'Freebuff will run commands on your behalf to help you build.\r\n'
            printf '\r\n'
            printf 'Directory %s\r\n' "$CWD"
            printf '\r\n'
            printf 'GLM 5.3 Flash · 59m left · 16.4K (2%%)      ✕ End session\r\n'
            printf '╭──────────────────────────────────────────────────────╮\r\n'
            if [ -n "$line" ]; then
                printf '│  ▍%s\r\n' "$line"
            else
                printf '│  ▍Enter a coding task or / for commands\r\n'
            fi
            printf '╰──────────────────────────────────────────────────────╯\r\n'
            if [ "$menu" -eq 1 ]; then
                printf '  /model      Choose the model for your next message\r\n'
            fi
        }

        draw_picker() {
            printf '\033[2J\033[H'
            printf '████████████████████████████████████████\r\n'
            printf '█                                                      █\r\n'
            printf '█   F  R  E  E  B  U  F  F                            █\r\n'
            printf '█                                                      █\r\n'
            printf '████████████████████████████████████████\r\n'
            printf '\r\nChoose a model to start a new session\r\n\r\n'
            if [ "$expanded" -eq 0 ]; then
                first_name=$(printf '%s\n' "$MODEL_NAMES" | head -n 1)
                if [ "$focus_toggle" -eq 1 ]; then
                    printf '│   %s · Deep reasoning · Reasoning: max · Images · NEW │\r\n' "$first_name"
                else
                    printf '│ › %s · Deep reasoning · Reasoning: max · Images · NEW │\r\n' "$first_name"
                fi
                printf '│                               %d Freebucks/hr                                │\r\n' "$(model_cost "$first_name")"
                printf '↓  See all %d models\r\n' "$N"
            else
                end=$(( scroll + V ))
                [ "$end" -gt "$N" ] && end=$N
                i=0
                printf '%s\n' "$MODEL_NAMES" | while IFS= read -r name; do
                    if [ "$i" -ge "$scroll" ] && [ "$i" -lt "$end" ]; then
                        if [ "$i" -eq "$focus" ]; then
                            printf '│ › %s · Deep reasoning · Reasoning: max · Images · NEW │\r\n' "$name"
                        else
                            printf '│   %s · Deep reasoning · Reasoning: max · Images · NEW │\r\n' "$name"
                        fi
                        printf '│                               %d Freebucks/hr                                │\r\n' "$(model_cost "$name")"
                    fi
                    i=$(( i + 1 ))
                done
                printf '↑  Show fewer\r\n'
            fi
        }

        draw_idle
        while true; do
            # Read one byte reliably: `read -n 1` cannot capture '\r' (Enter),
            # so use `dd bs=1 count=1` which passes every byte through.
            c=$(dd bs=1 count=1 2>/dev/null) || break
            [ -n "$c" ] || break # EOF: the PTY was closed.

            if [ "$mode" = "idle" ]; then
                case "$c" in
                    "$ESC")
                        # Discard a following arrow sequence (ESC [ X) so it is
                        # not appended to the typed line.
                        c2=$(dd bs=1 count=1 2>/dev/null) || c2=""
                        if [ "$c2" = "[" ]; then
                            dd bs=1 count=1 2>/dev/null >/dev/null || true
                        fi
                        ;;
                    "$CR")
                        if [ "$line" = "/model" ]; then
                            if [ "$submit_seen" -eq 0 ] && [ "${FAKE_FREEBUFF_NO_SWALLOW:-0}" -ne 1 ]; then
                                # The slash-autocomplete popup eats the first
                                # Enter: the menu stays open with `/model` still
                                # in the box, exactly like the real CLI.
                                submit_seen=1
                                menu=1
                            else
                                # The (second) Enter opens the picker. With
                                # FAKE_FREEBUFF_NO_SWALLOW=1 the FIRST Enter opens
                                # it, so a caller that nudges Enter again while
                                # the picker is already up hits a model row and
                                # the fake exits 1 below.
                                mode=picker
                                menu=0
                                expanded=0
                                focus=0
                                focus_toggle=0
                                scroll=0
                                draw_picker
                                continue
                            fi
                        elif [ "$line" = "/exit" ]; then
                            print_exit
                            exit 0
                        fi
                        ;;
                    *)
                        line="${line}${c}"
                        menu=0
                        ;;
                esac
                draw_idle
            else
                case "$c" in
                    "$ESC")
                        c2=$(dd bs=1 count=1 2>/dev/null) || continue
                        if [ "$c2" != "[" ]; then
                            # A bare Esc leaves the picker without selecting.
                            mode=idle
                            line=""
                            submit_seen=0
                            draw_idle
                            continue
                        fi
                        c3=$(dd bs=1 count=1 2>/dev/null) || continue
                        case "$c3" in
                            B)
                                if [ "$expanded" -eq 0 ]; then
                                    focus_toggle=1
                                elif [ "$focus" -lt "$N" ]; then
                                    focus=$(( focus + 1 ))
                                    if [ "$focus" -ge $(( scroll + V )) ]; then
                                        scroll=$(( focus - V + 1 ))
                                    fi
                                fi
                                ;;
                            A)
                                if [ "$expanded" -eq 0 ]; then
                                    focus_toggle=0
                                elif [ "$focus" -gt 0 ]; then
                                    focus=$(( focus - 1 ))
                                    if [ "$focus" -lt "$scroll" ]; then
                                        scroll=$focus
                                    fi
                                fi
                                ;;
                        esac
                        draw_picker
                        ;;
                    "$CR")
                        if [ "$expanded" -eq 0 ]; then
                            if [ "$focus_toggle" -eq 1 ]; then
                                expanded=1
                                focus=0
                                scroll=0
                            else
                                printf 'freebuff: Enter on a model row starts a session and spends Freebucks\n' >&2
                                exit 1
                            fi
                        elif [ "$focus" -eq "$N" ]; then
                            expanded=0
                            focus_toggle=0
                        else
                            printf 'freebuff: Enter on a model row starts a session and spends Freebucks\n' >&2
                            exit 1
                        fi
                        draw_picker
                        ;;
                    q|Q)
                        exit 0
                        ;;
                esac
            fi
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
    if [ "$FAKE_FREEBUFF_MODE" = "chip" ]; then
        # freebuff collapses a large paste into a chip above the box and does
        # not echo it. Show the chip BEFORE reading, and read in
        # non-canonical mode: canonical mode caps a line at MAX_CANON
        # (1024 bytes on macOS), which a long paste overflows.
        printf '\033[2J\033[H'
        print_idle_with_chip
        stty -icanon -echo icrnl min 1 time 0 2>/dev/null || true
        if ! read -r line; then stty sane 2>/dev/null || true; return 1; fi
        stty sane 2>/dev/null || true
    else
        printf '\033[2A\033[4C'
        read -r line || return 1
    fi
    printf '\n\n'
    ESC=$(printf '\033')
    line=$(printf '%s' "$line" | sed -e "s/${ESC}\[200~//g" -e "s/${ESC}\[201~//g")
    if [ "$FAKE_FREEBUFF_MODE" = "slow-busy" ]; then
        # Simulate freebuff consuming a large paste: the input box falls back
        # to the placeholder (paste is 'consumed') and busy is slow to appear.
        printf '\033[2J\033[H'
        print_idle
        sleep 6
    fi
    line_len=${#line}
    if [ "$line_len" -gt 1000 ]; then
        formatted=$(format_number "$line_len")
        print_paste_chip_and_box "$formatted"
    fi
    if [ "$line" = "/exit" ]; then
        print_exit
        exit 0
    fi
    if [ "$FAKE_FREEBUFF_MODE" = "prompt-gate" ]; then
        # Real incident (2026-10-07): freebuff showed the Freebucks gate when
        # the prompt was SUBMITTED, i.e. after startup had already reached Idle.
        # Re-draw the gate (the real captured screen, ANSI and all) and block;
        # the prompt is never consumed.
        printf '\033[2J\033[H'
        GATE_CAPTURE="$(dirname "$0")/../../../docs/research/captures/freebucks-gate-80x24.txt"
        if [ -f "$GATE_CAPTURE" ]; then
            cat "$GATE_CAPTURE"
        else
            print_gate_splash
        fi
        while true; do sleep 1; done
    fi
    if [ "$FAKE_FREEBUFF_MODE" = "prompt-stall" ]; then
        # The prompt neither goes busy nor is consumed: re-draw an idle screen
        # that still shows the submitted text in the box, then block. Forces the
        # generic submit timeout, whose screen dump blink must keep.
        printf '\033[2J\033[H'
        printf '  Freebuff will run commands on your behalf to help you build.\n\n'
        printf '  Directory %s\n\n' "$CWD"
        printf 'GLM 5.3 Flash · 59m left · 16.4K (2%%)      ✕ End session\n'
        printf '╭────╮\n'
        printf '│  ▍%s  │\n' "$line"
        printf '╰────╯\n'
        while true; do sleep 1; done
    fi
    TIMESTAMP=$(date -u +"%Y-%m-%dT%H-%M-%S.000Z")
    if [ -z "$CHAT_DIR" ]; then
        CHAT_DIR="$CHATS_DIR/$TIMESTAMP"
        mkdir -p "$CHAT_DIR"
    fi
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
                    if [ "$FAKE_FREEBUFF_MODE" = "slow-legacy-interrupt" ]; then
                        # Legacy freebuff builds: a Failed event whose message
                        # is exactly "user-interrupt".
                        write_log_line 'Agent execution failed' '{"error":{"name":"Error","message":"user-interrupt"}}'
                    else
                        # Real freebuff (captured 2026-10-03): an INFO
                        # "Agent run cancelled by user (abort error)" with no
                        # data.message field.
                        write_log_line 'Agent run cancelled by user (abort error)' '{"agentType":"main-agent","agentId":"main-agent","runId":"run-1","totalSteps":1,"messageHistory":[]}'
                    fi
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
