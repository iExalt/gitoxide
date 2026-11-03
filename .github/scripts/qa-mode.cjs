// Choose how much QA a Fork CI run does.
// Ordinary PRs get the routine (fast) selection: affected packages with every declared variant.
// Release PRs, pushes to dev and manual runs get the deep (exhaustive) set, so a release is never
// cut from code that only passed routine checks, and dev pushes keep every lane's cache warm.
const RELEASE_BRANCH_PREFIX = 'release-please--branches--';

function qaMode({ eventName, headRef }) {
  if (eventName === 'pull_request' && !headRef.startsWith(RELEASE_BRANCH_PREFIX)) return 'fast';
  return 'exhaustive';
}

if (require.main === module) {
  const mode = qaMode({ eventName: process.env.EVENT_NAME ?? '', headRef: process.env.HEAD_REF ?? '' });
  console.log(`mode=${mode}`);
}

module.exports = { qaMode };
