-- The step-up of a paired remote device: the instant until which its owner has confirmed grave
-- actions afresh. NULL (every existing row, and every newly paired device) means "never confirmed".
-- A column on the device row rather than a table: a device row IS one session (a pairing creates the
-- row and its `session_hash` together, nothing rotates the hash), so a new session is a new row with
-- NULL and cannot inherit this one, and revoking the device takes the step-up with it.
ALTER TABLE remote_devices ADD COLUMN stepup_until TEXT;
