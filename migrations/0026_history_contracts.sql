-- This release requires a coordinated upgrade of backend and clients.
INSERT INTO dm.contract_versions(family,version) VALUES('http',3),('websocket',5),('shared',2);
UPDATE dm.contract_versions SET status='sunset',deprecated_at=COALESCE(deprecated_at,clock_timestamp()),sunset_at=clock_timestamp()
WHERE (family,version) NOT IN (('http',3),('websocket',5),('shared',2));
