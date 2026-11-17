import argparse
import json
from pathlib import Path
from .core import scan, report, card
from .summary import summarize, expand_summary, SCHEMA


def main():
    parser = argparse.ArgumentParser(description="Conservative web-agent artifact observations")
    parser.add_argument("--version", action="version", version="0.1.0")
    sub = parser.add_subparsers(dest="command", required=True)
    s = sub.add_parser("scan")
    s.add_argument("targets", nargs="*")
    s.add_argument("--targets-file", type=Path, help="JSON mapping cohort names to domain lists")
    s.add_argument("--workers", type=int, default=6)
    s.add_argument("--spacing", type=float, default=0.3)
    s.add_argument("--timeout", type=float, default=8)
    s.add_argument("--max-bytes", type=int, default=65536)
    s.add_argument("-o", "--output", type=Path)
    for name in ("report", "octopus-card", "summarize"):
        p = sub.add_parser(name)
        p.add_argument("input", type=Path)
        p.add_argument("-o", "--output", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "scan":
            targets, cohorts = list(args.targets), {}
            if args.targets_file:
                for cohort, domains in json.loads(args.targets_file.read_text()).items():
                    targets.extend(domains)
                    cohorts.update({d: cohort for d in domains})
            if not targets:
                parser.error("Provide targets or --targets-file")
            result = scan(targets, args.workers, args.spacing, args.timeout, args.max_bytes, cohorts)
            text = json.dumps(result, indent=2, sort_keys=True) + "\n"
        else:
            result = json.loads(args.input.read_text())
            if result.get("schema") == SCHEMA:
                result = expand_summary(result)
            if args.command == "summarize":
                text = json.dumps(summarize(result), sort_keys=True, separators=(",", ":")) + "\n"
            else:
                text = report(result) if args.command == "report" else json.dumps(card(result), indent=2, sort_keys=True) + "\n"
        if args.output:
            args.output.write_text(text, encoding="utf-8")
        else:
            print(text, end="")
    except (ValueError, OSError, KeyError, TypeError) as exc:
        parser.error(str(exc))


if __name__ == "__main__":
    main()
