# Changelog

## [0.5.0](https://github.com/iExalt/gitoxide/compare/gitoxide-fork-v0.4.1...gitoxide-fork-v0.5.0) (2026-11-10)


### Features

* **gix:** add an opt-in non-interactive SSH policy ([7300dcd](https://github.com/iExalt/gitoxide/commit/7300dcd7fdf8e9d6e52606195ecf0c2861a3304e))


### Bug fixes

* **gix-odb:** retry loose-object publication races on macOS ([8e3a125](https://github.com/iExalt/gitoxide/commit/8e3a12555d387462f26ff1115eeb69040e2d4cc9))
* **internal-tools:** align QA tool requirements with fork pins ([4c1c5cd](https://github.com/iExalt/gitoxide/commit/4c1c5cdda70d7c3de72e62b0164ae21ca77ff998))


### Performance

* **internal-tools:** keep compatible feature tests on one CI shard ([977b324](https://github.com/iExalt/gitoxide/commit/977b3243c1e84aca9b14d63203825a9950253d7f))


### Fork maintenance

* **gix:** avoid redundant CI dependency setup ([f2062fd](https://github.com/iExalt/gitoxide/commit/f2062fd31374874d67f6986ae3d5e15178e9e82e))
* **gix:** preselect maintenance fixture blob inputs ([f1dda41](https://github.com/iExalt/gitoxide/commit/f1dda4160327bb8ada41f590fe952b0271af681f))
* **gix:** update development tools to latest releases ([3a25ce8](https://github.com/iExalt/gitoxide/commit/3a25ce8c849d181f3336f2808ba9ff3dfd254f1b))

## [0.4.1](https://github.com/iExalt/gitoxide/compare/gitoxide-fork-v0.4.0...gitoxide-fork-v0.4.1) (2026-11-06)


### Bug fixes

* **gix-odb:** touch a pack once for all the objects a handle freshens in it ([caaa20a](https://github.com/iExalt/gitoxide/commit/caaa20aa40725fa4356777872080b34315a83f2e))

## [0.4.0](https://github.com/iExalt/gitoxide/compare/gitoxide-fork-v0.3.0...gitoxide-fork-v0.4.0) (2026-11-05)


### ⚠ BREAKING CHANGES

* **gix:** check the objects of a fetch or clone when `fetch.fsckObjects` or `transfer.fsckObjects` asks for it
* **gix-protocol:** tell `fetch()` what consumes the pack which commits the `shallow` file will have

### Features

* **gitoxide:** serve local fetches and clones in process in the `gix` binary ([9456acd](https://github.com/iExalt/gitoxide/commit/9456acde073e8d0a2dfc4fad17db150da4593f64))
* **gix-fsck:** add Git's message ids, their severities and skip lists as `Options` ([3698d1c](https://github.com/iExalt/gitoxide/commit/3698d1cc7976cdc7d5e9d85f8c6a3aac53e6f4ca))
* **gix-fsck:** check commits, tags and trees like `index-pack --strict` and `unpack-objects --strict` do ([4ff0e7a](https://github.com/iExalt/gitoxide/commit/4ff0e7a01f2da789e576128c99f532106199d67b))
* **gix-fsck:** check the content of `.gitmodules` files like Git does ([0a00644](https://github.com/iExalt/gitoxide/commit/0a00644227f52c42e37f5a079f9df60b2f52c8eb))
* **gix-fsck:** look at received objects without requiring what they link to ([bc02102](https://github.com/iExalt/gitoxide/commit/bc0210212148ba0174599df3f34a32caf4a4f4e1))
* **gix-fsck:** say what `unpack-objects --strict` and `index-pack --strict` print about a received pack ([d3011c9](https://github.com/iExalt/gitoxide/commit/d3011c9b0df5c48fd73c273758389e9a9a280eb1))
* **gix-odb:** let an inspector look at a received pack before it is accepted ([90d9369](https://github.com/iExalt/gitoxide/commit/90d9369d62638d4f8f65d8916b7196de1b7a3be4))
* **gix-odb:** look at a pack that was written but isn't accepted yet with the `inspect` module ([88b3a47](https://github.com/iExalt/gitoxide/commit/88b3a47c3ec5e209217b82245c86fda4b83c014a))
* **gix-validate:** detect the files Git treats specially in every spelling a file system gives them ([e67dbeb](https://github.com/iExalt/gitoxide/commit/e67dbeb59d5e18f0fee0a0084a7cfdc15c7821bf))
* **gix:** check the objects of a fetch or clone when `fetch.fsckObjects` or `transfer.fsckObjects` asks for it ([ec898b2](https://github.com/iExalt/gitoxide/commit/ec898b2af20708d3ccceea76be7827cd6a009c29))
* **gix:** check the objects of a push when `receive.fsckObjects` or `transfer.fsckObjects` asks for it ([553fbce](https://github.com/iExalt/gitoxide/commit/553fbce4734b48fbc4f96b99a6badaf02e40c5a2))


### Bug fixes

* **gix-validate:** let malformed UTF-8 after a special file name end it for HFS+ comparisons ([e4fc78f](https://github.com/iExalt/gitoxide/commit/e4fc78fcc8eb8d59390e5b68cb3355bf9b74fab8))


### Changes

* **gix-protocol:** tell `fetch()` what consumes the pack which commits the `shallow` file will have ([91d3b3e](https://github.com/iExalt/gitoxide/commit/91d3b3e7a33784658011b1fb69ddf1e0f148fb92))


### Fork maintenance

* **gix-fsck:** describe in the comments how `.gitmodules` is read ([0928ac0](https://github.com/iExalt/gitoxide/commit/0928ac0d919be2d8caddd87dea8715d57b674985))

## [0.3.0](https://github.com/iExalt/gitoxide/compare/gitoxide-fork-v0.2.0...gitoxide-fork-v0.3.0) (2026-11-05)


### Features

* **gix:** write a set of objects as one pack with `Repository::write_objects_as_pack()` ([acd27bf](https://github.com/iExalt/gitoxide/commit/acd27bf75541aa2062c102b730af7cd78eb8fb4b))


### Bug fixes

* **gix:** change references inside a quarantine environment when the repository ignores the environment ([2e7c3c0](https://github.com/iExalt/gitoxide/commit/2e7c3c05e5978ad80a3bd9b76540ba178e083e96))

## [0.2.0](https://github.com/iExalt/gitoxide/compare/gitoxide-fork-v0.1.0...gitoxide-fork-v0.2.0) (2026-11-05)


### ⚠ BREAKING CHANGES

* **gix:** require Rust 1.99
* **gix-transport:** perform the requests of the reqwest backend with its asynchronous client
* **gix-ref:** `WriteReflog` is `#[non_exhaustive]` and has the new variant `Existing`, so matches over it need a wildcard arm. The files store no longer creates reflogs for references below `refs/worktree/` unless an edit forces it, and `core.logAllRefUpdates=false` and bare repositories append to reflogs that exist, where they used to write none.
* **gix-credentials:** run named credential helpers and `store` without `git`
* **gix-ref:** an edit with `PreviousValue::MustNotExist` fails when the reference exists in reftables, also if it holds the value that it would be written with. Callers that may create a reference that already has the value ask for `PreviousValue::ExistingMustMatch` with that value instead.
* **gix-ref:** an edit with `PreviousValue::MustNotExist` fails when the reference exists, also with the value that it would be written with. Every edit of a loose reference that can't be decoded fails as corrupt, where it used to treat the reference as absent: updates, deletions and the comparison with the packed reference of the same name. An expected value against such a reference (`MustExistAndMatch` or `ExistingMustMatch`) used to fail as not found, which `is_not_found()` reported, and now fails as corrupt. The `gix` porcelain inherits both changes: `Repository::reference()`, `tag_reference()`, `commit_as()` onto a reference without parents, the first notes commit and `gix worktree add` now refuse an existing reference even when it holds the same value.
* **gix-pack:** flush the pack and its index as `core.fsync` requires when writing a bundle
* **gix-odb:** flush loose objects as `core.fsync` and `core.fsyncObjectFiles` require
* **gix:** store a repository's references in the neutral `gix_ref::Store` instead of `file::Store`

### Features

* **gitoxide-core:** end `pack receive` from the interrupt handler of the program even if the server stopped responding ([0a2934f](https://github.com/iExalt/gitoxide/commit/0a2934f63d6abed1470d056b27d61bff47714341))
* **gitoxide:** add `ein init --ref-format` and apply Git's init defaults ([6af4e37](https://github.com/iExalt/gitoxide/commit/6af4e372590f2e1823df28c693216116e19c0462))
* **gix-credentials:** run named credential helpers and `store` without `git` ([0cacc56](https://github.com/iExalt/gitoxide/commit/0cacc56b0cccc2c54a165398f9b7af7a219e4e5b))
* **gix-fs:** add `fsync` to flush as `core.fsync` and `core.fsyncMethod` require ([ac2e40b](https://github.com/iExalt/gitoxide/commit/ac2e40ba2757fbe53f376ed5beb4216cc4b5874b))
* **gix-hook:** add a runner for Git hooks with Git-exact semantics ([3f0d3b1](https://github.com/iExalt/gitoxide/commit/3f0d3b1545376e9f15b0ee801e9b5647b6c74261))
* **gix-index:** expose what the resolve-undo extension remembers ([8dd0c02](https://github.com/iExalt/gitoxide/commit/8dd0c02b2592146bf8838ab7531ce873e34ec12a))
* **gix-odb:** flush loose objects as `core.fsync` and `core.fsyncObjectFiles` require ([7798f00](https://github.com/iExalt/gitoxide/commit/7798f004102e08b506fbd0d07c8da4891bce525e))
* **gix-odb:** receive packs into a quarantine like `git receive-pack` does ([4eb3ce5](https://github.com/iExalt/gitoxide/commit/4eb3ce54cd89ca247600ea2e053639a217c36e81))
* **gix-pack:** add a producer for the pack a receiver is missing ([819b250](https://github.com/iExalt/gitoxide/commit/819b25059afb72fdf7f035ace31354fc52f28446))
* **gix-pack:** create deltas and search for them with a sliding window, as `git pack-objects` does ([fce158e](https://github.com/iExalt/gitoxide/commit/fce158e49aecfaa7c0e3f0b04a9b34d78a6a4cf5))
* **gix-pack:** flush the pack and its index as `core.fsync` requires when writing a bundle ([5836258](https://github.com/iExalt/gitoxide/commit/58362585174ec65cfeb6a1ff507b2302036fbc1f))
* **gix-protocol:** add a blocking `push()` driver tested against real `receive-pack` ([d1185cb](https://github.com/iExalt/gitoxide/commit/d1185cb4dd6485c77d19aa838932cb3c2a83c71d))
* **gix-protocol:** add the I/O-free client codec for pushing to `receive-pack` ([ec94a0d](https://github.com/iExalt/gitoxide/commit/ec94a0df77f97f9ddc14352ce6b1ab2b2e21eb60))
* **gix-protocol:** make `push()` fail on incomplete packs instead of hanging ([6816cd6](https://github.com/iExalt/gitoxide/commit/6816cd69fa67f83d48af5f2b0137f3409fe5ecfe))
* **gix-protocol:** read and answer the requests of protocol version 2 the way `git upload-pack` does ([491ad1e](https://github.com/iExalt/gitoxide/commit/491ad1e6552273ba7bc9fb1981e9a1458731a68e))
* **gix-protocol:** read push requests and write advertisements and reports like `receive-pack` does ([10b14dd](https://github.com/iExalt/gitoxide/commit/10b14ddbdbd430c955d9cb394099268d16c3f519))
* **gix-ref:** add `StoreRef`, a borrowed view of a reference store that is not tied to files ([d80ac51](https://github.com/iExalt/gitoxide/commit/d80ac51040460d0bae42d37fc762df0aec9b55b8))
* **gix-ref:** decide the reflog of `HEAD` in reftables with the rules of the files store ([58f9aec](https://github.com/iExalt/gitoxide/commit/58f9aec4ec0c1c94fa4c04a4cc1d00f227730679))
* **gix-ref:** edit references that are stored in reftables ([4feb213](https://github.com/iExalt/gitoxide/commit/4feb213ebe57d3ac3620e594500d01cd73de9b80))
* **gix-ref:** expire and delete reflogs for files and reftable storage ([70de4e5](https://github.com/iExalt/gitoxide/commit/70de4e503db2a9231f0068ca120fd9b991e655ac))
* **gix-ref:** flush references while preparing and tell what a failed commit did ([f583cf7](https://github.com/iExalt/gitoxide/commit/f583cf73c674bb08a201e598285d94e066154f7c))
* **gix-ref:** keep appending to reflogs with `core.logAllRefUpdates=false`, and create none for private references ([935eade](https://github.com/iExalt/gitoxide/commit/935eade215bb2e57d550961eaec62743bc08b3e0))
* **gix-ref:** list the references that have a reflog with `StoreRef::reflog_names()` ([70d8b7a](https://github.com/iExalt/gitoxide/commit/70d8b7a995e7468b9ecbe7494fb0c33d3e69a789))
* **gix-ref:** log changes of the branch that `HEAD` points at in the reflog of `HEAD`, like Git ([02d25e2](https://github.com/iExalt/gitoxide/commit/02d25e28ee91706bba642de57ab9ec0c9afe95d0))
* **gix-ref:** name `view::NotFound` as the marker every kind of storage raises for a missing reference ([64d0f81](https://github.com/iExalt/gitoxide/commit/64d0f81a9ad496e8663e68d7d9d0c8b72c8b1913))
* **gix-ref:** pack references like `git pack-refs --all --prune` with `StoreRef::pack_refs()` ([9d742b2](https://github.com/iExalt/gitoxide/commit/9d742b20fbe7841e42d40e877b218b8535661c1b))
* **gix-ref:** re-export the recoverable transaction errors from `view::transaction` ([953a20c](https://github.com/iExalt/gitoxide/commit/953a20c5845e66b2bfafd28833640923b5a0f95e))
* **gix-ref:** read references that are stored in reftables ([caa3e62](https://github.com/iExalt/gitoxide/commit/caa3e626beb0346f713feb8c70094ef0852b3013))
* **gix-ref:** reject edits that fail on their own while the others go on, like `update-ref --batch-updates` ([004e59e](https://github.com/iExalt/gitoxide/commit/004e59e2216f981c44bad3baa2525c8556b7f323))
* **gix-ref:** report transaction states to an observer that can refuse them, like Git's hook ([3eec994](https://github.com/iExalt/gitoxide/commit/3eec994fe73908d8846705ce526927e0a7b79688))
* **gix-reftable:** add tables to a stack under the `tables.list` lock ([d6705a7](https://github.com/iExalt/gitoxide/commit/d6705a7d1b56bedbaf23990409c44cf574c85ae0))
* **gix-reftable:** compact stacks like Git does and clean up after them ([0a587b2](https://github.com/iExalt/gitoxide/commit/0a587b229cf5b6dd3391dfd903ce11193e35faa6))
* **gix-reftable:** let an addition own its stack and adopt a snapshot without reading ([e470f5a](https://github.com/iExalt/gitoxide/commit/e470f5ac77bdb1731f646197222e1b1ef58c38f2))
* **gix-reftable:** read and write reftable tables like Git does ([3975ff8](https://github.com/iExalt/gitoxide/commit/3975ff8717582225fd629800fbf3747a3c6ff3b7))
* **gix-reftable:** read stacks of tables like Git does ([fd9c299](https://github.com/iExalt/gitoxide/commit/fd9c299d5f3ecc022153e9a3d4feb5652fec945a))
* **gix-ref:** tell whether packing references is due, like `git pack-refs --auto` asks ([56d952f](https://github.com/iExalt/gitoxide/commit/56d952fa5e365b0ebe14aa56e6bf398833ce9d23))
* **gix-testtools:** add synthetic authenticated HTTP and SSH peers ([4455cdf](https://github.com/iExalt/gitoxide/commit/4455cdfd984f18f07bdac196adf047aa636e47e3))
* **gix-testtools:** let the HTTP peer keep what `git http-backend` said and pad a response ([89a80cc](https://github.com/iExalt/gitoxide/commit/89a80cc273754f78eb421f36e53b0d0bcf046baf))
* **gix-testtools:** limit the receive buffer of the connections of the HTTP peer ([3a57b4c](https://github.com/iExalt/gitoxide/commit/3a57b4c2ee29d66d90b0bbe1c911a5bf3f67e860))
* **gix-testtools:** serve several requests per connection in the HTTP peer on demand ([96d693b](https://github.com/iExalt/gitoxide/commit/96d693bc24354541ea84b0d2e3bd9e0c9f0327b9))
* **gix-transport:** apply the CA bundle, the verification and the protocol versions in the reqwest backend ([0e4da33](https://github.com/iExalt/gitoxide/commit/0e4da3388bb8c7db168ff61d74ee8d39ef4bad3b))
* **gix-transport:** apply the low-speed limits in the reqwest backend ([312e070](https://github.com/iExalt/gitoxide/commit/312e07001e82d96c51c951d28e6b7d66181d3fb7))
* **gix-transport:** end an operation from a flag with `InterruptHandle::watch()` ([0bd2e2d](https://github.com/iExalt/gitoxide/commit/0bd2e2d6f347a80a42cdb8be3e4b965238f95c37))
* **gix-transport:** end the requests of the HTTP backends from another thread with `Http::interrupt_handle()` ([420bf74](https://github.com/iExalt/gitoxide/commit/420bf74a3404af398059b22eb4ed50e2e5cba79f))
* **gix-transport:** end what a process or socket transport blocks on from another thread with `TransportWithoutIO::interrupt_handle()` ([8f4e154](https://github.com/iExalt/gitoxide/commit/8f4e154e2e19148b166e0cf32821407718167a03))
* **gix-transport:** offer `rustls` with the `ring` provider for the reqwest backend ([526338e](https://github.com/iExalt/gitoxide/commit/526338eb6e38ace4b3c2d4335ad7981c6f7b21ce))
* **gix-transport:** use proxies, their credentials and `no_proxy` in the reqwest backend ([33725db](https://github.com/iExalt/gitoxide/commit/33725db1ae750f736a301332bda4036146862904))
* **gix-transport:** wait for the server process to finish instead of killing it ([c287c81](https://github.com/iExalt/gitoxide/commit/c287c813c7291a4d3a84ff24258ef0179edd259b))
* **gix-utils:** let reads of a pipe that wait for data fail when a flag is set ([f2eba6b](https://github.com/iExalt/gitoxide/commit/f2eba6b95e620808f89279f43e15ededaf0dcabe))
* **gix:** add `core.fsync`, `core.fsyncMethod` and `core.fsyncObjectFiles` to the configuration tree ([1dfa263](https://github.com/iExalt/gitoxide/commit/1dfa2634b69665c6f85fa9b3aeb73f398d3df79a))
* **gix:** add `edit_references_with_hooks()` that runs the `reference-transaction` hook like `git update-ref` ([5d098e1](https://github.com/iExalt/gitoxide/commit/5d098e149764f03541fa538f37ceee51b434bfcc))
* **gix:** add `init_or_open()` that leaves an existing repository untouched ([9c4843a](https://github.com/iExalt/gitoxide/commit/9c4843a05366fe7b967075fed222f83ffdfe7543))
* **gix:** add `Repository::hook()` to find and run hooks as Git does ([ec7b486](https://github.com/iExalt/gitoxide/commit/ec7b4869f9a6e5c8c24ceb75b0d0bea9b082de37))
* **gix:** add `Repository::is_ancestor()` that answers like `git merge-base --is-ancestor` ([36e9634](https://github.com/iExalt/gitoxide/commit/36e9634f74418e5089e25ee39ee0961cad12d7c0))
* **gix:** add typed `gc.*` and `maintenance.*` configuration keys ([b56e846](https://github.com/iExalt/gitoxide/commit/b56e846579a7e52d5251fd29533d12efbe3cb06d))
* **gix:** check that the objects of a push are complete, like `git receive-pack` asks `rev-list` to ([cadb9b9](https://github.com/iExalt/gitoxide/commit/cadb9b91774d66add788844526b136b5d22e8868))
* **gix:** configure how reftable stacks are written with `reftable.*`, `core.fsync` and `core.sharedRepository` ([e8553af](https://github.com/iExalt/gitoxide/commit/e8553af8f971e10d69cf1faae6e933122fe94f9a))
* **gix:** create repositories that store references as reftables when asked to, like Git ([aab0363](https://github.com/iExalt/gitoxide/commit/aab03630a365ae5d4372c0cd1e917282d73aca28))
* **gix:** expire reflog entries like `git reflog expire --all` with `Repository::expire_reflogs()` ([dcc4b53](https://github.com/iExalt/gitoxide/commit/dcc4b53d9bfff3d55e91a84692329c1d150a19e7))
* **gix:** find the objects that are still wanted, like `git prune` does ([f78e95d](https://github.com/iExalt/gitoxide/commit/f78e95dc07cea4cf302fdc861d44a223eee160c6))
* **gix:** handle reftable repositories in `gix` by default and in the full presets of `gitoxide` ([24f1948](https://github.com/iExalt/gitoxide/commit/24f1948230b213a6a292f9e5b15f24e0eb33cf41))
* **gix:** ignore the environment that redirects a repository or injects configuration when opening it ([37a291e](https://github.com/iExalt/gitoxide/commit/37a291e33ec0569644baa748069dfa4574ee69b9))
* **gix:** let a flag end a fetch or push whose server stopped responding ([480fe62](https://github.com/iExalt/gitoxide/commit/480fe6224635411671558a1ee199d0e78438be85))
* **gix:** list references by whole path components with `references().under()` ([c4dd37b](https://github.com/iExalt/gitoxide/commit/c4dd37b99b21698e757d480c44268f8caf5a7d92))
* **gix:** open repositories that store references as reftables when built with `reftable` ([5be3a86](https://github.com/iExalt/gitoxide/commit/5be3a8631d3af8e6956266a52fb4b4f30df9346b))
* **gix:** prune stale tracking references while fetching, after the update succeeded ([2c28c48](https://github.com/iExalt/gitoxide/commit/2c28c48122201babcf2e44336ec9d863e9e6f506))
* **gix:** push references with leases to a remote and send the objects they need ([92368ab](https://github.com/iExalt/gitoxide/commit/92368abf8e753b86b9c958e9a28ff9e898513bea))
* **gix:** push to a repository on the same machine without starting `git-receive-pack` ([42026d7](https://github.com/iExalt/gitoxide/commit/42026d7775babe481edfe403a123df4e7f162aa0))
* **gix:** put objects that a repack writes into deltas, like `git repack -a -d` does ([ea7e2e3](https://github.com/iExalt/gitoxide/commit/ea7e2e3d28e47c7c971e4024d5254db91281f2a7))
* **gix:** put unreachable objects into a cruft pack when repacking, like `git repack --cruft` does ([54d140a](https://github.com/iExalt/gitoxide/commit/54d140a4f83b1047be81665b4c5d0c40613496ad))
* **gix:** receive from a client that is shallow, rejecting what would make the receiver shallow ([4ec70bf](https://github.com/iExalt/gitoxide/commit/4ec70bff7b3246fa8fc056b48c448783a09f051d))
* **gix:** receive into a shallow repository from a client that has the whole history ([280e30a](https://github.com/iExalt/gitoxide/commit/280e30af8a25b51567fb3f0fb43a4d6944209846))
* **gix:** receive pushes into a local repository like `git receive-pack` does ([c10533c](https://github.com/iExalt/gitoxide/commit/c10533ce8d8ae5405248549fe12b7d932b924301))
* **gix:** receive pushes into a namespace like `git receive-pack` does with `GIT_NAMESPACE` ([cc87209](https://github.com/iExalt/gitoxide/commit/cc87209ce5be7fe46bb13a2c4445609d171c5a76))
* **gix:** reconcile an uncertain push with what the remote has ([41fb7f9](https://github.com/iExalt/gitoxide/commit/41fb7f97daa1708ddf09cbc214450a55634024e1))
* **gix:** remove unreachable loose objects, like `git prune` and `git prune-packed` do ([0d2a0a9](https://github.com/iExalt/gitoxide/commit/0d2a0a9c452b315d55bb523bbd99322c586a13e6))
* **gix:** repack objects and remove the packs they replace, like `git repack -a -d` and `-d` do ([f2f55a4](https://github.com/iExalt/gitoxide/commit/f2f55a471df9df32e32bbc7a770fcd3799e2a441))
* **gix:** roll up the smallest packs and all loose objects geometrically, like `git repack -d --geometric` does ([3edd08d](https://github.com/iExalt/gitoxide/commit/3edd08d6a9f4cdab56e199c00bfc530045e9a6c4))
* **gix:** run automatic maintenance after the native local transport received a push ([4c0701b](https://github.com/iExalt/gitoxide/commit/4c0701b53a7dd7b6f64be14dfb7fbc1bcc99e760))
* **gix:** run garbage collection and maintenance tasks like `git gc` and `git maintenance run` ([83f9fb4](https://github.com/iExalt/gitoxide/commit/83f9fb4fabc68eb385daaab1e51d42b6c724c6a4))
* **gix:** run the `pre-push` hook of a push with `Prepare::with_pre_push_hook()` ([cd5c720](https://github.com/iExalt/gitoxide/commit/cd5c72014c829c8d60de5863076dfc5c17e6030a))
* **gix:** run the `pre-receive`, `update`, `post-receive` and `post-update` hooks when receiving a push ([e4c50b7](https://github.com/iExalt/gitoxide/commit/e4c50b7e77004137bf398bc11fad2898e864ca6d))
* **gix:** run the `push-to-checkout` hook for `receive.denyCurrentBranch=updateInstead` instead of refusing it ([4aa5f15](https://github.com/iExalt/gitoxide/commit/4aa5f151ef2515c05f5cf62e99eafa79e68f8b2e))
* **gix:** run the `reference-transaction` hook when a receiver updates references ([9ce4675](https://github.com/iExalt/gitoxide/commit/9ce467571fdf3d5612b3b6723488f5739bb0630a))
* **gix:** serve `ls-refs` and `fetch` of protocol version 2 from a repository, in the process, like `git upload-pack` ([d5eca27](https://github.com/iExalt/gitoxide/commit/d5eca278512c68a78a9e9fc7d12ca289e83cc13b))
* **gix:** take the shallow boundary of a client into the `shallow` file with `receive.shallowUpdate` ([677342b](https://github.com/iExalt/gitoxide/commit/677342b69be32bb91047ea0840955514aad9906b))
* **gix:** tell the client when a hook of a push was killed by a signal ([bda86ec](https://github.com/iExalt/gitoxide/commit/bda86ecda89cc8e50e7341d45536852ce3bf5906))
* **gix:** update the work tree and index of a pushed branch with `receive.denyCurrentBranch=updateInstead` ([63cb29a](https://github.com/iExalt/gitoxide/commit/63cb29a37f7a634f8f57fe23991bb1e8d0f653bc))
* **gix:** use `sirocco` by default, with `zlib-rs` when the default features are off ([cb4047e](https://github.com/iExalt/gitoxide/commit/cb4047ef099009fa37559f4f76eddf60d19e517d))


### Bug fixes

* **gix-config:** replace a value where it is when setting a key that an earlier section has ([2e55b87](https://github.com/iExalt/gitoxide/commit/2e55b872ea560d4669c467d6a285e6203a2fe59c))
* **gix-hook:** start no hook after one that couldn't be started ([edd152d](https://github.com/iExalt/gitoxide/commit/edd152d8c7a732b225e43858432f8b3ec7a9be7c))
* **gix-odb:** create a missing object directory again instead of failing the write ([85aba04](https://github.com/iExalt/gitoxide/commit/85aba04fcdc31f9307b003a84f9fd8e337628c92))
* **gix-odb:** drop the duplicate `filetime` dependency that two merged changes introduced ([e240fe3](https://github.com/iExalt/gitoxide/commit/e240fe3124c4cd60bc39c8a71115ca021a553f90))
* **gix-odb:** look for a pack again when a loose object is gone by the time it is read ([d5ead82](https://github.com/iExalt/gitoxide/commit/d5ead8215c265d44f5079fba7be96fde8894c64f))
* **gix-pack:** honor the chunk size of a single-threaded `iter_from_counts()` ([4e98781](https://github.com/iExalt/gitoxide/commit/4e9878110c10cf299813c268486da0293f58563a))
* **gix-pack:** keep QA green when Git deflates with a different zlib ([ec5ffbd](https://github.com/iExalt/gitoxide/commit/ec5ffbd53eec9ae5e664181967f5e50034b8f5e1))
* **gix-pack:** make the delta encoder choose like Git's `diff-delta`, so deltas are as small ([393f6d0](https://github.com/iExalt/gitoxide/commit/393f6d0df3b0f63de21aa2910ed0fb91f1a21ccb))
* **gix-protocol:** don't ask credential helpers to authenticate over `ssh` ([4c9cbcf](https://github.com/iExalt/gitoxide/commit/4c9cbcfc0b180156f37cf64ce15c1a1d03203074))
* **gix-protocol:** report a producer failure after a complete pack as an unknown outcome ([ce02d70](https://github.com/iExalt/gitoxide/commit/ce02d70a874b8e76a04b2036d1512a97f6c6f87f))
* **gix-protocol:** wait for the server to finish after a push without side-band ([f4bdeea](https://github.com/iExalt/gitoxide/commit/f4bdeeae72d25349f3039e868f2e653c62d78487))
* **gix-ref:** check the expected id of a deletion through a symbolic reference against its referent ([bd98b89](https://github.com/iExalt/gitoxide/commit/bd98b89b5950cb51cd46a9a172eadeba52e9331d))
* **gix-ref:** clean reflog messages of reftables like those of the files store ([de86910](https://github.com/iExalt/gitoxide/commit/de86910fa9d7891b0ee93141a533bc8467efb039))
* **gix-ref:** keep `packed-refs` locked until the loose references are gone ([d3e38b4](https://github.com/iExalt/gitoxide/commit/d3e38b4b53e26144e066c26f19a22e5b8358c595))
* **gix-ref:** leave the log-only `HEAD` edit out of what reftable transactions report ([9b65886](https://github.com/iExalt/gitoxide/commit/9b6588670bf9b6186e45c3679e91bdb372a4759c))
* **gix-ref:** link from the documentation of the reftable module to its store by full path ([18443a2](https://github.com/iExalt/gitoxide/commit/18443a253fe6c109911455a4618ba706150b4795))
* **gix-ref:** refuse to create a reference in reftables that exists already, like Git ([0e32f2e](https://github.com/iExalt/gitoxide/commit/0e32f2ef7bc5f18dcb4b8774f2ef08129a4fa0a3))
* **gix-ref:** refuse to create what exists and to edit what can't be read, like Git ([28b71e1](https://github.com/iExalt/gitoxide/commit/28b71e14e6240ee6778ec741b779a3bdcfac163c))
* **gix-ref:** reject conflicting reference names while preparing a transaction ([5ed71d7](https://github.com/iExalt/gitoxide/commit/5ed71d71344543be845d25eb00ac48d2bdbc4ee9))
* **gix-ref:** report rejected edits to observers in the order Git does when others go through symbolic references ([0d7465c](https://github.com/iExalt/gitoxide/commit/0d7465c2ccd11bf1301fb66d80bb93beb45148fb))
* **gix-refspec:** expand wildcard sources to what precedes the wildcard when listing remote references ([6f133ab](https://github.com/iExalt/gitoxide/commit/6f133aba26943c79e39022ede2f1b767ba1f8220))
* **gix-ref:** write reflog messages the way Git cleans them ([a47bfc8](https://github.com/iExalt/gitoxide/commit/a47bfc8634c38ae6f5cf093df598a3d37a1ae92a))
* **gix-ref:** write the records of a reftable transaction at one update index, like Git ([cb7269a](https://github.com/iExalt/gitoxide/commit/cb7269a3a838504a8dd4b7b1cb874d8fa051c6fe))
* **gix-transport:** don't cut a streamed upload of the `curl` backend short after a buffered one ([07e33af](https://github.com/iExalt/gitoxide/commit/07e33aff40ba69575fb880a23fd9616057a236ba))
* **gix-transport:** don't fail the curl backend on the proxy's first refusal, and let curl use proxy credentials from helpers ([90afe64](https://github.com/iExalt/gitoxide/commit/90afe641c369a3cdd76b59a5cd94e7441998a67f))
* **gix-transport:** learn that a push needs credentials before sending it ([ae825fe](https://github.com/iExalt/gitoxide/commit/ae825fe20708370fa31fa8bae7d9530d1e883bc0))
* **gix-transport:** tell a consumer of the `curl` backend why a transfer failed when its pipe is full ([fa31d09](https://github.com/iExalt/gitoxide/commit/fa31d09adbd438086a8ba2e16b3f5f8f19d9ee5f))
* **gix:** don't fail to find what is reachable while `HEAD` points to a branch that isn't born yet ([52a2df4](https://github.com/iExalt/gitoxide/commit/52a2df4b1e4b8514dc34e3271f71bf5424cd6d1c))
* **gix:** finish the removal of a pack that a killed repack was in the middle of ([4fb61f0](https://github.com/iExalt/gitoxide/commit/4fb61f02113bb5a36c750f840228afcc6093474c))
* **gix:** keep the submodule symlink fixture from pointing at `/` ([8c8d048](https://github.com/iExalt/gitoxide/commit/8c8d048a14e88203fbbb3e1efeb07d7ce4ebf1a6))
* **gix:** make the marker of a new pack appear whole or not at all, and let the kill test accept the time of a loose object being written ([108064c](https://github.com/iExalt/gitoxide/commit/108064c0206218cfcd3d571fbcd343210edeaad9))
* **gix:** refuse every reference edit while `GIT_QUARANTINE_PATH` is set, like Git ([bfe90d6](https://github.com/iExalt/gitoxide/commit/bfe90d6e9f0518b2413ea2467ef1dc7305882477))
* **gix:** refuse every reference operation on repositories with unsupported ref storage ([08ac289](https://github.com/iExalt/gitoxide/commit/08ac289f14eb22ac3cbc8138da76b7974b61c826))
* **gix:** refuse to create repositories that need reference storage other than files ([bb0bb53](https://github.com/iExalt/gitoxide/commit/bb0bb538fbeb25b7afbba75c4acc1e34eed28c51))
* **gix:** remove the pack that a killed repack moved into place without its index ([70a0ddf](https://github.com/iExalt/gitoxide/commit/70a0ddfa7cb31a5ea36c7436dfce8706e0ccdc2d))
* **gix:** report the failure of packing references too when repacking fails after it ([a12d87a](https://github.com/iExalt/gitoxide/commit/a12d87ab25f8432843e06ee0e8b2755b0a04f224))
* **gix:** satisfy clippy in the tests that only build with the `receive` and `upload` features ([37eb944](https://github.com/iExalt/gitoxide/commit/37eb944b62a7d7357064b27b929c6955e24e5dab))
* **gix:** touch an existing object when a write skips it, so that pruning keeps it ([be78d78](https://github.com/iExalt/gitoxide/commit/be78d784a13583f1732b3c1ae4972d0c4e4eb59f))
* **gix:** treat a `core.abbrev` longer than a hash as the whole hash, like Git ([2e05e29](https://github.com/iExalt/gitoxide/commit/2e05e2992b2383bff779255a72857298fb983d11))


### Changes

* **gix-transport:** perform the requests of the reqwest backend with its asynchronous client ([a280073](https://github.com/iExalt/gitoxide/commit/a2800730f69c98a354f1c32cfe53c3353f7befbf))
* **gix:** store a repository's references in the neutral `gix_ref::Store` instead of `file::Store` ([1bc6fd6](https://github.com/iExalt/gitoxide/commit/1bc6fd610d0fd9972ef2fb66ced92edca36a56a1))


### Fork maintenance

* **gix-hash:** optimise `sha2` in the dev and test profiles like the other hashing crates ([73fc878](https://github.com/iExalt/gitoxide/commit/73fc878b0f9451008e74ea9d3537ac161a687c7b))
* **gix-protocol:** test complete pushes against `receive-pack` with packs from `gix-pack` ([86d76d3](https://github.com/iExalt/gitoxide/commit/86d76d3c6caa8b63a15d372ccf37dc2281a4f6e1))
* **gix-ref:** compare ref transactions with Git's file system effects under `strace` ([9abc281](https://github.com/iExalt/gitoxide/commit/9abc28157e948318384867c8fbaaa2e92d77c37e))
* **gix-reftable:** test tables against Git in both directions ([ec76709](https://github.com/iExalt/gitoxide/commit/ec7670955b93a15a845ca25eb30925152123e4bf))
* **gix-testtools:** fail the tests that trace with `strace` in CI instead of skipping them ([1cd3afc](https://github.com/iExalt/gitoxide/commit/1cd3afcde71a72fbc65cbd1992ac35be2c3d38c7))
* **gix-transport:** satisfy the current clippy in the tests of the HTTP transports ([cb05a45](https://github.com/iExalt/gitoxide/commit/cb05a456e356c215d0e340675156390267a52639))
* **gix:** lint the `receive` and `upload` features in `just clippy` ([ec1aaec](https://github.com/iExalt/gitoxide/commit/ec1aaecf61820a13d87a2653ccf93a964822413b))
* **gix:** require Rust 1.99 ([eda41d6](https://github.com/iExalt/gitoxide/commit/eda41d654ccd1fd090f22f0ffd4ebb39d465be88))
* **internal-tools:** treat Markdown below `docs/` as prose in the QA planner ([6a73c7d](https://github.com/iExalt/gitoxide/commit/6a73c7d7096d97f06618bba24c496302a9ea0c72))

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
