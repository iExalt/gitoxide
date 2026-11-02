module.exports = async function sync(github, core) {
  const { data: source } = await github.rest.repos.getBranch({
    owner: 'GitoxideLabs', repo: 'gitoxide', branch: 'main',
  });
  // GitHub rejects non-fast-forwards atomically; never rewrite a diverged mirror.
  await github.rest.git.updateRef({
    owner: 'iExalt', repo: 'gitoxide', ref: 'heads/upstream/main',
    sha: source.commit.sha, force: false,
  });
  core.info(`upstream/main matches GitoxideLabs/gitoxide:main at ${source.commit.sha}`);
};
