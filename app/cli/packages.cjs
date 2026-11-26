const { inspectLocalPackageDrift } = require("../electron/localPackages.cjs");

function formatPackageCheck(report) {
  const lines = [report.ok ? "Packages: clean" : "Packages: review required"];
  for (const pkg of report.packages) {
    lines.push(`${pkg.id}: receipt=${pkg.receipt_state}${pkg.update_available ? " update available" : ""}`);
    for (const file of pkg.files) lines.push(`  ${file.reason}: ${file.registry_kind}/${file.owner}/${file.path}`);
    for (const issue of pkg.issues) lines.push(`  ${issue.code}${issue.member ? ": " + issue.member : ""}`);
  }
  for (const transfer of report.transfers) lines.push(`transfer ${transfer.registry_kind}/${transfer.owner}: ${transfer.from} -> ${transfer.to} (${transfer.state}: ${transfer.reason})`);
  for (const issue of report.issues) lines.push(`${issue.code}${issue.member ? ": " + issue.member : ""}`);
  return lines.join("\n") + "\n";
}

async function checkPackages(roots, json, stdout) {
  const report = await inspectLocalPackageDrift(roots);
  stdout.write(json ? JSON.stringify(report, null, 2) + "\n" : formatPackageCheck(report));
  return report.ok ? 0 : 1;
}

module.exports = { checkPackages, formatPackageCheck };
