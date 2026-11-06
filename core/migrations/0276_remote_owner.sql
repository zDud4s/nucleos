-- The "owner" level of a paired remote device: the owner's own laptop reaching what only the host
-- reached until now. A column rather than a new access level, so the CHECKs on `access_level`
-- stay as they are and an API token can never be an owner. Only a device with `access_level =
-- 'admin'` can carry it (enforced where the code is minted and again where it is read), and every
-- existing row is an ordinary device.
ALTER TABLE remote_devices ADD COLUMN owner INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pairing_codes ADD COLUMN owner INTEGER NOT NULL DEFAULT 0;
