# Cardigan - A Apple<->Fastmail Contacts Sync

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.3](https://github.com/szinn/Cardigan/compare/v0.1.1..v0.1.3) - 2026-10-04

### Features

- _(cg-core)_ Execute photo writes and record photo state - ([80952a1](https://github.com/szinn/Cardigan/commit/80952a135c4cd8532c916f60c968f614603d15cc))
- _(cg-core)_ Plan photo changes per side - ([19b0783](https://github.com/szinn/Cardigan/commit/19b078352faf18a29629bdd21bf27980f4d854c9))
- _(cg-core)_ Download iCloud photos before planning - ([f60d2ed](https://github.com/szinn/Cardigan/commit/f60d2ed4be59f15261bd1df08c859d04cb18dadc))
- _(cg-core)_ Add the PhotoFetcher port and iCloud adapter - ([a83dcb8](https://github.com/szinn/Cardigan/commit/a83dcb812134492e507726097944b18e04cef3b7))
- _(cg-core)_ Fit photos into iCloud's card size limit - ([46ef0d3](https://github.com/szinn/Cardigan/commit/46ef0d3563af2aaf6c616426b1e6aafaa944634b))
- _(cg-core)_ Add photo identities and inline-photo rewriting - ([3426afa](https://github.com/szinn/Cardigan/commit/3426afa1d11d00bd451952ce813a1340a5ff04ba))
- _(cg-core)_ Relink groups after a replayed recreate; report unmapped members - ([0e0c376](https://github.com/szinn/Cardigan/commit/0e0c3762c4f318a1248d4b559bc53d4586a9b341))
- _(cg-core)_ Relink copied groups after a re-UID - ([e702603](https://github.com/szinn/Cardigan/commit/e702603b2d89daca827342f54346762565d2cfc3))
- _(cg-core)_ Add Op::CopyGroup - ([7db2a68](https://github.com/szinn/Cardigan/commit/7db2a68a7833f7a38e99966f0294f41f6a825197))
- _(cg-core)_ Rewrite group member UIDs in a vCard - ([33c278e](https://github.com/szinn/Cardigan/commit/33c278eb91e7594b0b3d075e1fd89cab33776864))
- _(cg-database)_ Record per-side photo state - ([ecfdb39](https://github.com/szinn/Cardigan/commit/ecfdb39754457da5936032f3ef87bafb002fd725))

### Bug Fixes

- _(cg-core)_ CG-15 final-review follow-ups - ([76dc80e](https://github.com/szinn/Cardigan/commit/76dc80edb80b76151a200b0710e8606f51e37ecb))

### Testing

- _(cg-core)_ End-to-end photo sync against an iCloud-mode fake - ([18f5e0f](https://github.com/szinn/Cardigan/commit/18f5e0f612e1f50939f77fbbb7734d0ee6bba4d0))
- _(cg-core)_ Cover unmapped-member reporting and relink edge cases - ([dad20df](https://github.com/szinn/Cardigan/commit/dad20df8bd4ca4dffb8aa27013f7ee05eb5cbeb7))
- _(integration-tests)_ Update stale expectations after CG-15 and CG-18 - ([603f1b8](https://github.com/szinn/Cardigan/commit/603f1b825740f5d9b8141b395f523542aab006b1))

### Miscellaneous Tasks

- _(cargo)_ Remove postgres and mysql support - ([2bc7123](https://github.com/szinn/Cardigan/commit/2bc71233e51c34515af1a5a32dcfbe8c72abaec2))

## [0.1.1](https://github.com/szinn/Cardigan/compare/v0.1.0..v0.1.1) - 2026-09-29

### Bug Fixes

- _(cg-core)_ Normalize BDAY/ANNIVERSARY dates and ignore VND-63-SENSITIVE-CONTENT-CONFIG in the canonical hash - ([d1fac3e](https://github.com/szinn/Cardigan/commit/d1fac3e61a7fe01bfe2e4caae6349c2bd95e6bb6))

### Miscellaneous Tasks

- _(release)_ Fix release script run location - ([a73fe90](https://github.com/szinn/Cardigan/commit/a73fe90d368542054b376cad816953e6ec36637a))

## [0.1.0] - 2026-09-29

### Features

- _(cardigan)_ Wire the SyncService into sync and dry-run - ([92faf7d](https://github.com/szinn/Cardigan/commit/92faf7d2c9033784bb1b715e2ea76c5b4076bc25))
- _(cardigan)_ Print the dry-run plan and fail sync --once when blocked - ([eccb79c](https://github.com/szinn/Cardigan/commit/eccb79cd27d873a7e2f647a53087eb5efc02a4cb))
- _(cardigan)_ Run the daemon loop on cg-core cycle types and honour retry_after - ([4a53d66](https://github.com/szinn/Cardigan/commit/4a53d668526da5921e03e1f56a59eba3028cfc83))
- _(cardigan)_ Add the dump command - ([39821e3](https://github.com/szinn/Cardigan/commit/39821e3197f562dee61d31f8393605446ef5d9f7))
- _(cardigan)_ Add the dump module - ([05a92c0](https://github.com/szinn/Cardigan/commit/05a92c09ab6d55b52c24f48688c8d3e199a56662))
- _(cardigan)_ Wire cg-carddav into the binary - ([3f47451](https://github.com/szinn/Cardigan/commit/3f474511067cf26ed43ff28a67abefbd5b492714))
- _(cardigan)_ Replace server with sync and dry-run commands - ([a7fa0b2](https://github.com/szinn/Cardigan/commit/a7fa0b26768cf4c6d13365a4a41aaa106a8162ae))
- _(cardigan)_ Add sync poll loop and CycleRunner seam - ([87852c4](https://github.com/szinn/Cardigan/commit/87852c4771cafdc9b1fed14cca83df68132e91d7))
- _(cardigan)_ Load configuration from CARDIGAN_* environment variables - ([4b06195](https://github.com/szinn/Cardigan/commit/4b06195395f9d5f35c6c61b1d933b5630e98e83c))
- _(cg-carddav)_ Implement conditional put and delete - ([9b49259](https://github.com/szinn/Cardigan/commit/9b4925946b133dce1e1786c04453ccab941c55a7))
- _(cg-carddav)_ Implement batched addressbook-multiget - ([4248cd1](https://github.com/szinn/Cardigan/commit/4248cd10edebb7d763a7c2f3d8649fa8b84b6921))
- _(cg-carddav)_ Implement changes_since and list_etags - ([807a26a](https://github.com/szinn/Cardigan/commit/807a26aaf81d53611eb4c1b29e3c2e587088c0fa))
- _(cg-carddav)_ Add discovery and the CardDavAddressBook shell - ([c336dba](https://github.com/szinn/Cardigan/commit/c336dba1d3ef76cc09c1be24d8433d44ae3e5194))
- _(cg-carddav)_ Add XML request/response layer and href canonicalization - ([269cc9d](https://github.com/szinn/Cardigan/commit/269cc9d57c71456e66b860f1c343a59615b5f6b9))
- _(cg-carddav)_ Add crate skeleton, HTTP client and error mapping - ([1a54db8](https://github.com/szinn/Cardigan/commit/1a54db87de40c5aa6efd68debd0af429d03ed76b))
- _(cg-core)_ List synced nameless duplicates in the report for manual cleanup - ([aba1b39](https://github.com/szinn/Cardigan/commit/aba1b398629294771e16c2a775ca5973f50cc6db))
- _(cg-core)_ Report and log likely nameless duplicates separately from ambiguous skips - ([383862a](https://github.com/szinn/Cardigan/commit/383862a06f030e7e55683c70ffcc619af28ea137))
- _(cg-core)_ Skip nameless cards that duplicate a card the other side holds (CG-18) - ([adfe42f](https://github.com/szinn/Cardigan/commit/adfe42f5ea527a63f75b73f120aae2de01cdedf2))
- _(cg-core)_ List held cross-pair deletes in the baseline report - ([a095597](https://github.com/szinn/Cardigan/commit/a0955971607ef2045c14502138e31ae6281c4d98))
- _(cg-core)_ Hold deletes that cross pairs for one contact (CG-17) - ([be0a27a](https://github.com/szinn/Cardigan/commit/be0a27a15459c36a3a5d4073da845baef8d09f87))
- _(cg-core)_ Add the DeleteHeld diagnostic and a held_deletes summary count - ([97a72a7](https://github.com/szinn/Cardigan/commit/97a72a7e658252e073f283b6e00791076ab181ab))
- _(cg-core)_ Add MatchKeys::may_be_same_contact for the cross-pair delete guard - ([95a9c64](https://github.com/szinn/Cardigan/commit/95a9c64aa4a6bb50f66c9f8f10d5967252cdfc4d))
- _(cg-core)_ Journal recreates before the delete and replay interrupted ones - ([f71859f](https://github.com/szinn/Cardigan/commit/f71859fd8c4d7171f4267424a0ce2fc88d3d748e))
- _(cg-core)_ Add the pending_recreates journal repository - ([2f5d690](https://github.com/szinn/Cardigan/commit/2f5d690eceb094356b96e560f275c91844c53ded))
- _(cg-core)_ Persist skips, tokens and last-seen, add idle cycles, --reset and cycle logging - ([74bb245](https://github.com/szinn/Cardigan/commit/74bb24535a2c7537a348739bf8d22550bfe7860e))
- _(cg-core)_ Apply conflicts and baseline recreates, recording conflicts before the push - ([758ff52](https://github.com/szinn/Cardigan/commit/758ff5225ba87beebaa983eca80a666a3b4016b9))
- _(cg-core)_ Apply create, update, delete and state-only ops with per-card failures - ([a8f49d6](https://github.com/szinn/Cardigan/commit/a8f49d6d7c327d175e8fbd8a9913b5628f5036d9))
- _(cg-core)_ Add SyncService discovery, listing, snapshots and dry-run - ([8072e87](https://github.com/szinn/Cardigan/commit/8072e870c98e36a8b9d151601d1eae10173737db))
- _(cg-core)_ Add an in-memory state store for tests and diagnostic failure reasons - ([82e1fad](https://github.com/szinn/Cardigan/commit/82e1fad7fe02a026a449acab00aa24b77fb635e2))
- _(cg-core)_ Add the baseline report and plan_cycle - ([5e43a53](https://github.com/szinn/Cardigan/commit/5e43a53e0565bf829efdd3fe3712a72b0bafc253))
- _(cg-core)_ Add pairing pass 3, skips and unique copies - ([b66e77f](https://github.com/szinn/Cardigan/commit/b66e77f8621153d1b20b8cb50e036637eb10cdd7))
- _(cg-core)_ Add baseline pairing passes 1 and 2 - ([bd8522f](https://github.com/szinn/Cardigan/commit/bd8522f41cd68cf9a9407e1248095dcc49cef619))
- _(cg-core)_ Add VCard::with_uid, group member helpers and MatchKeys::name_key - ([3fb50db](https://github.com/szinn/Cardigan/commit/3fb50db5562a7a950dda0ac297045e508967e34d))
- _(cg-core)_ Add the mass-deletion guard - ([2fbde3d](https://github.com/szinn/Cardigan/commit/2fbde3d9ec64e1d98ae1474a4dd6a52f07107d81))
- _(cg-core)_ Add the sync planner - ([d9c3c08](https://github.com/szinn/Cardigan/commit/d9c3c0802507733d3a8e9300244a40b9fb393685))
- _(cg-core)_ Classify each side's snapshot against sync state - ([acb12fe](https://github.com/szinn/Cardigan/commit/acb12fe95a4ecf01ed77b3d58bf73b2fe03ada99))
- _(cg-core)_ Add sync plan types and fetch lists - ([601f27f](https://github.com/szinn/Cardigan/commit/601f27f917c186ec04d37d9c10f8a72df2f9573b))
- _(cg-core)_ Add in-memory AddressBook fake - ([93b240b](https://github.com/szinn/Cardigan/commit/93b240b1b3e990daba106235c98d3fe5aa401c5f))
- _(cg-core)_ Add AddressBook port and error taxonomy - ([7a86209](https://github.com/szinn/Cardigan/commit/7a8620997a7eca227e490e127312264e36f7324b))
- _(cg-core)_ Add vCard display identity and match keys - ([0ffe487](https://github.com/szinn/Cardigan/commit/0ffe487f757157ce197f07046ce768c12820dbf8))
- _(cg-core)_ Add vCard photo size and stripping - ([64d9bd4](https://github.com/szinn/Cardigan/commit/64d9bd411f6af50aa1a66a29f128c95086c36c75))
- _(cg-core)_ Add versioned canonical vCard hash - ([1155436](https://github.com/szinn/Cardigan/commit/115543678afb3645423d113a5a0196116ef8bbaf))
- _(cg-core)_ Add lossless vCard 3.0 parser - ([d2c16bb](https://github.com/szinn/Cardigan/commit/d2c16bb6419f21bd2c12ce585792006469c2a7c3))
- _(cg-core)_ Add contact identifier types and Side/ConflictWinner - ([5cf812e](https://github.com/szinn/Cardigan/commit/5cf812e1411933141255f25b750d15c30519fb28))
- _(cg-database)_ Add baseline skip repository - ([8dc0a11](https://github.com/szinn/Cardigan/commit/8dc0a1104993448296bb29dbccfe37ab5b3cbd00))
- _(cg-database)_ Add card failure repository and backoff policy - ([d58f6a1](https://github.com/szinn/Cardigan/commit/d58f6a12d61ef3de75fabd140ac64dd8810bb120))
- _(cg-database)_ Add conflict history repository - ([8230918](https://github.com/szinn/Cardigan/commit/82309186bc35cc723173a9d9b6f4293ae066fbbc))
- _(cg-database)_ Add endpoint sync-state repository - ([cebb074](https://github.com/szinn/Cardigan/commit/cebb07493c4ec2fcced127ea329f81bad3ce1265))
- _(cg-database)_ Add contact sync-state repository - ([cc7b840](https://github.com/szinn/Cardigan/commit/cc7b8405d22b89a892c6a6b96de0b6be76e84397))

### Bug Fixes

- _(cardigan)_ Clearer rate-limit log, 8 s shutdown timeout - ([2fa8a26](https://github.com/szinn/Cardigan/commit/2fa8a26b44022537f9456860de2d779a83379093))
- _(cardigan)_ Address CG-1 review findings - ([3c4cf87](https://github.com/szinn/Cardigan/commit/3c4cf87fa012aeee405d2a3b5b47cd2e591a7af3))
- _(cg-carddav)_ Log the host when a CardDAV server rejects the credentials - ([2393a93](https://github.com/szinn/Cardigan/commit/2393a931b373d111ec515d48cb2998ff004380e8))
- _(cg-carddav)_ Enforce transport policy on discovered hrefs and tighten edge cases - ([60229a1](https://github.com/szinn/Cardigan/commit/60229a14f31909f5470afea5e4f1e7bfb8f39921))
- _(cg-core)_ Skip a nameless card that duplicates a named card copied in the same cycle (CG-18) - ([f257405](https://github.com/szinn/Cardigan/commit/f2574052704765775f8804a9007c4a1d399ab8ed))
- _(cg-core)_ Count likely duplicates in the report header, list synced duplicate pairs once, name the match in the warning - ([78d8b8f](https://github.com/szinn/Cardigan/commit/78d8b8f7ff6da6073a25662dfb8eeb1dc0b52fa7))
- _(cg-core)_ Say which copy to edit to resolve a held delete and point to dry-run - ([86f0bc3](https://github.com/szinn/Cardigan/commit/86f0bc39a9ffc9b965e6c3354579ba77cf18fcbe))
- _(cg-core)_ Don't replay a recreate for a contact deleted on iCloud - ([14c3891](https://github.com/szinn/Cardigan/commit/14c38917991ee8056ae93574878d74f1ec80faab))
- _(cg-core)_ Close baseline pairing gaps from review - ([506aa6d](https://github.com/szinn/Cardigan/commit/506aa6d35e8ce38936506b31f033451f038101b9))
- _(cg-core)_ Close sync planner gaps from review - ([dd31fa9](https://github.com/szinn/Cardigan/commit/dd31fa9a265c52cfb7c7065cb9a432ac27c25f3a))
- _(cg-core)_ Address CG-4 final review findings - ([a7d3e05](https://github.com/szinn/Cardigan/commit/a7d3e05b01cd732f0cd3506bfd7ee0a43de20f7b))
- _(cg-core)_ Address CG-2 final review findings - ([73f221e](https://github.com/szinn/Cardigan/commit/73f221e1a9d0f7bd15f7857e5cabfd302cf8a6ba))
- _(cg-database)_ Address CG-3 final review findings - ([002608b](https://github.com/szinn/Cardigan/commit/002608bd4194cc8cb587906b15ff53cc2fdf3a16))
- _(cg-database)_ Enforce read-only transactions on SQLite - ([57d0e0c](https://github.com/szinn/Cardigan/commit/57d0e0c17f2766eaeb1ff8351efe85e7a365bc54))

### Refactor

- _(cardigan)_ Remove CARDIGAN_MAX_PHOTO_BYTES - ([612cbd8](https://github.com/szinn/Cardigan/commit/612cbd8abada3783a0dad842e73d5ea2213a8717))
- _(cardigan)_ Drop BookBoss log filter leftovers - ([83d5174](https://github.com/szinn/Cardigan/commit/83d5174919deb4c5d14038171bf906f901068ace))

### Documentation

- _(claude)_ Add integration rule, real-server notes and environment gotchas - ([c78ea05](https://github.com/szinn/Cardigan/commit/c78ea05b462a56c437e86312c6c783d871f957fd))

### Testing

- _(cardigan)_ Pin broken-pipe and multiget arguments in dump tests - ([3b3755c](https://github.com/szinn/Cardigan/commit/3b3755cb9575695341dfc1875834d2f583e0df1b))
- _(cg-core)_ Add vCard fidelity fixtures and hash contract snapshots - ([7db3561](https://github.com/szinn/Cardigan/commit/7db3561ae316a36d4eea6cbfc1b4c4cd45b9212f))
- _(integration-tests)_ Hold deletes that cross pairs against Radicale - ([2bed3bf](https://github.com/szinn/Cardigan/commit/2bed3bff447d5af195eabe4b2707e4e8cfe0606e))
- _(integration-tests)_ Assert recovery writes and conflicts, and make failures debuggable - ([9914669](https://github.com/szinn/Cardigan/commit/99146695ff7f935be79a1b400be49ee2f990e5be))
- _(integration-tests)_ Crash convergence at four points and a lost response - ([c7481e8](https://github.com/szinn/Cardigan/commit/c7481e88b96f21e2d062d767d7b1cac0c1ea4411))
- _(integration-tests)_ A real 412 resolved as a conflict, and sync-token fallback - ([227dc00](https://github.com/szinn/Cardigan/commit/227dc00fb56a86573d8851515b2510a70b5996d7))
- _(integration-tests)_ Propagation, baseline, multiget batching and idle against Radicale - ([b64d8d4](https://github.com/szinn/Cardigan/commit/b64d8d472688655b86c6044ccaf07879f5166893))
- _(integration-tests)_ Add the Radicale harness and a first propagation test - ([b4cbe48](https://github.com/szinn/Cardigan/commit/b4cbe489eafc6f5d063f89787b4abe0d6b8dc3e2))

### Miscellaneous Tasks

- _(cg-core)_ Remove unused mock repository helpers and keep the repository service in CoreServices - ([9e1f13f](https://github.com/szinn/Cardigan/commit/9e1f13fc7cf4d42a99a2dacb6490d5af1f877508))
- _(deny)_ Allow common permissive licenses and add a mise deny task - ([bbeec51](https://github.com/szinn/Cardigan/commit/bbeec51195768e5d34e0715640f65275cd4380de))
- _(release)_ Add release script - ([3cbfcfe](https://github.com/szinn/Cardigan/commit/3cbfcfe37f9ce3a6263b92f2b7e0f9e037daa332))
