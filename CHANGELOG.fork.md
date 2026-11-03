# Changelog

## 0.1.0 (2026-11-03)


### ⚠ BREAKING CHANGES

* **gix:** version every crate in lockstep at the fork version ([#8](https://github.com/iExalt/gitoxide/issues/8))

### Changes

* **gix:** version every crate in lockstep at the fork version ([#8](https://github.com/iExalt/gitoxide/issues/8)) ([8f44064](https://github.com/iExalt/gitoxide/commit/8f440645c78745a16b74cb85c11081ecb8314e4b))


### Fork maintenance

* **gix:** cut fork CI and local test time with parallel lanes, a routine/deep split and a test loop ([#4](https://github.com/iExalt/gitoxide/issues/4)) ([a2571af](https://github.com/iExalt/gitoxide/commit/a2571afc8d1cbf11bbb2749dce6f71a0af83a666))
* **gix:** establish fork development and release PR infrastructure ([#1](https://github.com/iExalt/gitoxide/issues/1)) ([935100a](https://github.com/iExalt/gitoxide/commit/935100af12519a6a78c14d4ded9ca5203d843a4a))
* **gix:** fail rebase-dev fixture tests in CI instead of skipping them on old git ([bdddde5](https://github.com/iExalt/gitoxide/commit/bdddde56cf13c9ab24815ec3bcb97248ef1ba9ad))
* **gix:** keep Markdown headings when rebase-dev continues past a reused resolution ([84bb6dc](https://github.com/iExalt/gitoxide/commit/84bb6dcbfa1d8db983af5ecf5a404b7e420cec24))
* **gix:** keep upstream-controlled code away from the rebase signing key and App tokens ([b7fe40e](https://github.com/iExalt/gitoxide/commit/b7fe40e0dad897c31a29388139e0ef2ef4e2a780))
* **gix:** land PRs by rebase merge and rebase dev onto normalized upstream ([aa66e31](https://github.com/iExalt/gitoxide/commit/aa66e31188bcfc126d71780df91951a835d5230f))
* **gix:** rebase `dev` onto normalized upstream with a signed, CI-validated workflow ([1ce5240](https://github.com/iExalt/gitoxide/commit/1ce5240f155275e59edf0deb0acd3bd5e52b682e))
* **gix:** resolve upstream edits to workflows the fork deleted during `rebase-dev` ([258aac2](https://github.com/iExalt/gitoxide/commit/258aac2eed348fdda361b4e6ff70e6c213d85aba))
* **gix:** run workflow lint in the basics lane instead of the plan job ([8b2cea9](https://github.com/iExalt/gitoxide/commit/8b2cea9db9719a68ad0d7fa9c41e636dbce3834d))
* **gix:** set up node in rebase-dev's publish job without reading the repository's mise config ([cd81644](https://github.com/iExalt/gitoxide/commit/cd816440cd87624ed44ef617d5956b21658f0f1a))
* **gix:** skip git-lfs smudging in rebase-dev's publish job ([1979aeb](https://github.com/iExalt/gitoxide/commit/1979aeb967e6f477bb491ea439f11e7c89d79aff))
* **gix:** start release notes at the rebased upstream base ([3ab8d69](https://github.com/iExalt/gitoxide/commit/3ab8d69bee2911b2d73dda74da588dd52f1d2d8e))
* **gix:** tag and publish a fork release when its release PR merges ([#10](https://github.com/iExalt/gitoxide/issues/10)) ([5b98598](https://github.com/iExalt/gitoxide/commit/5b985984985c248d4e9c0b19a5a8300b30d97ef7))
* **gix:** trim routine CI queueing with a shallow plan checkout and a combined basics lane ([#6](https://github.com/iExalt/gitoxide/issues/6)) ([0db0fbe](https://github.com/iExalt/gitoxide/commit/0db0fbe6026e1f9a3585dca3f1a076770d5856ce))
