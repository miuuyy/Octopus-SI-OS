"""Body-free, deterministic scan metadata tables for public datasets."""
from .core import statistics

SCHEMA = "agent_ready_radar.summary"
BODY_FIELDS = {"body", "body_base64"}
REDIRECT_COLUMNS = ("url", "status", "location", "latency_ms")


def summarize(data):
    if data.get("schema") != "agent_ready_radar.scan" or data.get("schema_version") != 1:
        raise ValueError("Expected agent_ready_radar.scan version 1")
    columns = sorted({"redirect_chain"} | {k for d in data["domains"] for p in d["probes"]
                                          for k in p if k not in BODY_FIELDS})
    result = {k: v for k, v in data.items() if k not in ("schema", "domains")}
    result.update(schema=SCHEMA, probe_columns=columns,
                  redirect_columns=list(REDIRECT_COLUMNS), statistics=statistics(data))
    result["domains"] = []
    for domain in data["domains"]:
        rows = []
        for probe in domain["probes"]:
            metadata = {k: v for k, v in probe.items() if k not in BODY_FIELDS}
            metadata["redirect_chain"] = [[hop.get(k) for k in REDIRECT_COLUMNS]
                                          for hop in probe.get("redirect_chain", [])]
            rows.append([metadata.get(k) for k in columns])
        result["domains"].append({**domain, "probes": rows})
    return result


def expand_summary(data):
    if data.get("schema") != SCHEMA or data.get("schema_version") != 1:
        raise ValueError("Expected agent_ready_radar.summary version 1")
    columns = data["probe_columns"]
    if (not isinstance(columns, list) or not all(isinstance(k, str) for k in columns)
            or len(set(columns)) != len(columns) or BODY_FIELDS.intersection(columns)
            or "redirect_chain" not in columns
            or data["redirect_columns"] != list(REDIRECT_COLUMNS)):
        raise ValueError("Invalid summary columns")
    result = {k: v for k, v in data.items()
              if k not in ("probe_columns", "redirect_columns", "statistics", "domains")}
    result["schema"] = "agent_ready_radar.scan"
    result["domains"] = []
    for domain in data["domains"]:
        probes = []
        for row in domain["probes"]:
            if not isinstance(row, list) or len(row) != len(columns):
                raise ValueError("Invalid summary probe row")
            probe = dict(zip(columns, row))
            chain = probe["redirect_chain"]
            if (not isinstance(chain, list)
                    or any(not isinstance(hop, list) or len(hop) != len(REDIRECT_COLUMNS)
                           for hop in chain)):
                raise ValueError("Invalid summary redirect row")
            probe["redirect_chain"] = [dict(zip(REDIRECT_COLUMNS, hop)) for hop in chain]
            probes.append(probe)
        result["domains"].append({**domain, "probes": probes})
    if statistics(result) != data["statistics"]:
        raise ValueError("Summary statistics do not match probe metadata")
    return result
