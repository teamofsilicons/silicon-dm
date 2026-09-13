-- Usage is local contract-lifecycle state, isolated with each data plane.
CREATE TABLE dm.contract_versions (
    family text NOT NULL,
    version integer NOT NULL CHECK(version > 0),
    status text NOT NULL DEFAULT 'active' CHECK(status IN ('active','deprecated','sunset')),
    introduced_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    deprecated_at timestamptz,
    last_request_at timestamptz,
    requests bigint NOT NULL DEFAULT 0,
    sunset_at timestamptz,
    PRIMARY KEY(family,version),
    CHECK(status='active' OR deprecated_at IS NOT NULL)
);
INSERT INTO dm.contract_versions(family,version) VALUES('http',1),('websocket',3),('shared',1);
