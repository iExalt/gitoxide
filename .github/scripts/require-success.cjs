// Fail a required gate unless every job it depends on succeeded.
// A skipped or cancelled dependency fails the gate, because rulesets treat a
// skipped required check as passing.
function failures(needs) {
  const entries = Object.entries(needs);
  if (entries.length === 0) return ['the gate has no dependencies'];
  return entries
    .filter(([, job]) => job.result !== 'success')
    .map(([name, job]) => `${name}: ${job.result}`);
}

if (require.main === module) {
  const failed = failures(JSON.parse(process.env.NEEDS ?? '{}'));
  if (failed.length > 0) {
    console.error(`Required jobs did not succeed:\n${failed.join('\n')}`);
    process.exit(1);
  }
  console.log('Every required job succeeded.');
}

module.exports = { failures };
