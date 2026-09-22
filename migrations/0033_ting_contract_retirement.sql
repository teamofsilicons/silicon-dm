-- DM HTTP remains authoritative; Ting owns notification transport.
UPDATE dm.contract_versions
SET status='sunset',
    deprecated_at=COALESCE(deprecated_at,clock_timestamp()),
    sunset_at=COALESCE(sunset_at,clock_timestamp())
WHERE family IN ('websocket','shared');
