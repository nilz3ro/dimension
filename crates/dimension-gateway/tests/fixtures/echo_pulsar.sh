#!/bin/sh
# Echo incoming stdin in 3 chunks with delays to exercise gRPC streaming.
# Used by pulsar_e2e.rs::pulsar_grpc_streaming_no_buffering test.
#
# When invoked as a bundle entrypoint:
#   1. Reads the message payload from stdin
#   2. Writes 3 chunks to stdout with a 100ms delay between each
#   3. Exits 0 (triggering ACK on the Pulsar consumer)
#
# The dimension-agent streams each stdout line as a separate BackendEvent::Message
# chunk via the gRPC Execute stream, which is then forwarded to collect_response.
# This fixture exercises the end-to-end streaming path without buffering.
read -r MSG
echo "chunk-1: $MSG"
sleep 0.1
echo "chunk-2: $MSG"
sleep 0.1
echo "chunk-3: $MSG"
