#!/usr/bin/env bash
# verify-e2e-bridge.sh — End-to-end verification for the deployed Telegram bridge.
#
# Usage:
#   ./scripts/verify-e2e-bridge.sh <BRIDGE_URL>
#
# Examples:
#   ./scripts/verify-e2e-bridge.sh http://localhost:3000
#   ./scripts/verify-e2e-bridge.sh https://bridge.example.com
#
# The script exercises three automated checks:
#   1. GET  /health           — liveness probe
#   2. POST /telegram/webhook — mock Telegram update (expects {"ok":true})
#   3. POST /agent/callback   — mock agent callback   (expects {"ok":true})
#
# After automated checks it prints a human-in-the-loop test procedure.
set -euo pipefail

# ── Argument parsing ──────────────────────────────────────────────────────────

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <BRIDGE_URL>"
  echo "  BRIDGE_URL — Base URL of the bridge (e.g. http://localhost:3000)"
  exit 1
fi

BRIDGE_URL="${1%/}"  # strip trailing slash
PASS=0
FAIL=0

# ── Helpers ───────────────────────────────────────────────────────────────────

pass() { PASS=$((PASS + 1)); echo "  ✅ PASS: $1"; }
fail() { FAIL=$((FAIL + 1)); echo "  ❌ FAIL: $1"; }

check_response() {
  local label="$1" url="$2" method="$3" data="${4:-}" expect="${5:-}"
  local http_code body

  if [[ "$method" == "GET" ]]; then
    http_code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$url" 2>/dev/null) || true
    body=$(curl -s --max-time 10 "$url" 2>/dev/null) || true
  else
    http_code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 \
      -X POST -H 'Content-Type: application/json' -d "$data" "$url" 2>/dev/null) || true
    body=$(curl -s --max-time 10 \
      -X POST -H 'Content-Type: application/json' -d "$data" "$url" 2>/dev/null) || true
  fi

  if [[ "$http_code" != "200" ]]; then
    fail "$label — expected HTTP 200, got $http_code"
    echo "       Response body: $body"
    return
  fi

  if [[ -n "$expect" ]]; then
    if echo "$body" | grep -qF "$expect"; then
      pass "$label (HTTP $http_code, body contains '$expect')"
    else
      fail "$label — HTTP 200 but body missing '$expect'"
      echo "       Response body: $body"
    fi
  else
    pass "$label (HTTP $http_code)"
  fi
}

# ── Check 1: Health endpoint ──────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "  Telegram Bridge E2E Verification"
echo "  Target: $BRIDGE_URL"
echo "═══════════════════════════════════════════════════════════════"
echo ""
echo "── Check 1: GET /health ──────────────────────────────────────"

check_response "Health check" "$BRIDGE_URL/health" "GET" "" '"status":"ok"'

# ── Check 2: POST /telegram/webhook (mock Telegram update) ───────────────────

echo ""
echo "── Check 2: POST /telegram/webhook (mock TelegramUpdate) ────"

# Payload matches TelegramUpdate from telegram-bridge/src/types.ts:
#   { update_id: number, message?: { message_id, chat: { id }, text?, from?: { id, first_name } } }
TELEGRAM_UPDATE='{
  "update_id": 100000001,
  "message": {
    "message_id": 42,
    "chat": { "id": 12345678 },
    "text": "Hello from verify-e2e-bridge",
    "from": { "id": 99999, "first_name": "E2E-Test" }
  }
}'

check_response "Telegram webhook" "$BRIDGE_URL/telegram/webhook" "POST" "$TELEGRAM_UPDATE" '"ok":true'

# ── Check 3: POST /agent/callback (mock AgentCallbackEvent) ──────────────────

echo ""
echo "── Check 3: POST /agent/callback (mock AgentCallbackEvent) ──"

# Payload matches AgentCallbackEvent from telegram-bridge/src/types.ts:
#   { session_id, event_type, event_id, content: { role, content }, timestamp }
AGENT_CALLBACK='{
  "session_id": "e2e-test-session-001",
  "event_type": "Message",
  "event_id": "evt-e2e-001",
  "content": {
    "role": "assistant",
    "content": "Hello from the agent! This is a test callback."
  },
  "timestamp": "2026-01-01T00:00:00.000Z"
}'

check_response "Agent callback" "$BRIDGE_URL/agent/callback" "POST" "$AGENT_CALLBACK" '"ok":true'

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════════════════════════"
echo "  Results: $PASS passed, $FAIL failed"
echo "═══════════════════════════════════════════════════════════════"

# ── Human-in-the-loop test procedure ─────────────────────────────────────────

echo ""
echo "── Manual End-to-End Test Procedure ──────────────────────────"
echo ""
echo "The automated checks above verify that the bridge HTTP endpoints"
echo "accept well-formed requests. To verify the full Telegram → Dimension"
echo "→ Telegram loop, follow these manual steps:"
echo ""
echo "  1. Ensure the bridge is running and reachable at $BRIDGE_URL"
echo "  2. Confirm Cloudflare tunnel routes to the bridge port (default 3000)"
echo "  3. Register the Telegram webhook (replace YOUR_BOT_TOKEN and TUNNEL_URL):"
echo ""
echo "     curl -s https://api.telegram.org/botYOUR_BOT_TOKEN/setWebhook \\"
echo "       -d url=TUNNEL_URL/telegram/webhook"
echo ""
echo "  4. Open Telegram, find your bot, send a text message"
echo "  5. Verify bridge logs show 'telegram webhook received' with the update_id"
echo "  6. Verify Dimension gateway receives a POST /run request"
echo "  7. Wait for the agent to complete — bridge logs show 'agent callback received'"
echo "  8. Verify the bot replies in Telegram with the agent's response"
echo ""
echo "  Session continuity test:"
echo "  9. Send a second message in the same Telegram chat"
echo " 10. Verify the agent receives history from the first exchange"
echo " 11. Verify the reply reflects awareness of the prior conversation"
echo ""
echo "  Message buffering test:"
echo " 12. Send two messages rapidly before the first invocation completes"
echo " 13. Verify bridge logs show the second message is queued"
echo " 14. After the first invocation completes, verify the queued message"
echo "     triggers a new invocation with the buffered text"
echo ""

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
