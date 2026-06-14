-- Add session metadata (arbitrary JSON) and webhook URL for event notifications.
ALTER TABLE sessions ADD COLUMN metadata JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE sessions ADD COLUMN webhook_url TEXT NULL;

-- Expand event_type CHECK to include metadata_change.
ALTER TABLE session_events DROP CONSTRAINT IF EXISTS session_events_event_type_check;
ALTER TABLE session_events ADD CONSTRAINT session_events_event_type_check
  CHECK (event_type IN ('message', 'tool_call', 'tool_result', 'metadata_change'));
