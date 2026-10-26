// Real output of the agmen CLI, captured by tests/e2e/tests/it_m1.rs
// (it_plugin_contract_matches_the_cli) and read by register.test.tsx, so
// the plugin's parsers run against what the CLI prints. The worktree path,
// ids and handles of the run are replaced by fixed ones. Do not edit it by
// hand; capture it again with
//   AGMEN_E2E_UPDATE_FIXTURE=1 cargo nextest run -p agmen-e2e --profile it -E 'test(it_plugin_contract)'
export const CLI_OUTPUT = {
  "watch": [
    "{\"kind\":\"self\",\"identity_id\":\"andysol01\",\"handle\":\"andy/sol\",\"owner_id\":\"andyhuman01\"}",
    "{\"kind\":\"connection\",\"connected\":true}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8EH6EMWCKFVKJ3DHYGPJ\",\"reply_to\":null,\"to\":[],\"mentions\":[],\"items\":[],\"lane\":null,\"body\":\"\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.047006Z\",\"verb\":\"FOUND\",\"data\":{\"title\":\"contract ui\",\"lane\":\"L-UI\",\"severity\":null},\"stream\":\"feed\",\"seq\":1,\"from\":\"adamcoord01\",\"from_handle\":\"adam/coord\",\"from_kind\":\"coordinator\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.072Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8ER436ACXKVXT6810XEA\",\"reply_to\":\"01M5VN8EH6EMWCKFVKJ3DHYGPJ\",\"to\":[],\"mentions\":[],\"items\":[\"B-1\"],\"lane\":\"L-UI\",\"body\":\"B-1 (L-UI)\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.268Z\",\"verb\":\"ITEM\",\"data\":{\"key\":\"B-1\"},\"stream\":\"feed\",\"seq\":2,\"from\":\"agmenhub0\",\"from_handle\":\"hub\",\"from_kind\":\"hub\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.268Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8ESM1CZ003FM9ZKN3ZW1\",\"reply_to\":null,\"to\":[],\"mentions\":[],\"items\":[],\"lane\":null,\"body\":\"\",\"valid_until\":\"2026-10-26T08:30:31.316189Z\",\"sent_at\":\"2026-10-26T08:29:31.316187Z\",\"verb\":\"CLAIM\",\"data\":{\"item\":\"B-1\"},\"stream\":\"feed\",\"seq\":3,\"from\":\"adamcoord01\",\"from_handle\":\"adam/coord\",\"from_kind\":\"coordinator\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.339Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8ETMV0YFKXWKWFP6T3WE\",\"reply_to\":\"01M5VN8ESM1CZ003FM9ZKN3ZW1\",\"to\":[],\"mentions\":[],\"items\":[\"B-1\"],\"lane\":\"L-UI\",\"body\":\"B-1 granted to adam/coord until 2026-10-26T08:59:31.334Z\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.348Z\",\"verb\":\"GRANT\",\"data\":{\"item\":\"B-1\",\"lease_until\":\"2026-10-26T08:59:31.334Z\"},\"stream\":\"feed\",\"seq\":4,\"from\":\"agmenhub0\",\"from_handle\":\"hub\",\"from_kind\":\"hub\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.348Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8EVT8TR4HYGYTS9HGS04\",\"reply_to\":null,\"to\":[],\"mentions\":[],\"items\":[],\"lane\":null,\"body\":\"\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.386948Z\",\"verb\":\"FOUND\",\"data\":{\"title\":\"contract core\",\"lane\":\"L-CORE\",\"severity\":null},\"stream\":\"feed\",\"seq\":5,\"from\":\"andysol01\",\"from_handle\":\"andy/sol\",\"from_kind\":\"agent\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.41Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8EWR03QXQ76EDN0MMRM0\",\"reply_to\":\"01M5VN8EVT8TR4HYGYTS9HGS04\",\"to\":[],\"mentions\":[],\"items\":[\"B-2\"],\"lane\":\"L-CORE\",\"body\":\"B-2 (L-CORE)\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.416Z\",\"verb\":\"ITEM\",\"data\":{\"key\":\"B-2\"},\"stream\":\"feed\",\"seq\":6,\"from\":\"agmenhub0\",\"from_handle\":\"hub\",\"from_kind\":\"hub\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.416Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8EY6P64XANJ47QNGW3FK\",\"reply_to\":null,\"to\":[],\"mentions\":[],\"items\":[],\"lane\":null,\"body\":\"\",\"valid_until\":\"2026-10-26T08:30:31.46226Z\",\"sent_at\":\"2026-10-26T08:29:31.462258Z\",\"verb\":\"CLAIM\",\"data\":{\"item\":\"B-2\"},\"stream\":\"feed\",\"seq\":7,\"from\":\"andysol01\",\"from_handle\":\"andy/sol\",\"from_kind\":\"agent\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.484Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8EZ3A0NSJDV74RT52XYM\",\"reply_to\":\"01M5VN8EY6P64XANJ47QNGW3FK\",\"to\":[],\"mentions\":[],\"items\":[\"B-2\"],\"lane\":\"L-CORE\",\"body\":\"B-2 granted to andy/sol until 2026-10-26T08:59:31.478Z\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.491Z\",\"verb\":\"GRANT\",\"data\":{\"item\":\"B-2\",\"lease_until\":\"2026-10-26T08:59:31.478Z\"},\"stream\":\"feed\",\"seq\":8,\"from\":\"agmenhub0\",\"from_handle\":\"hub\",\"from_kind\":\"hub\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.491Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8EWR03QXQ76EDN0MMRM0\",\"reply_to\":\"01M5VN8EVT8TR4HYGYTS9HGS04\",\"to\":[],\"mentions\":[],\"items\":[\"B-2\"],\"lane\":\"L-CORE\",\"body\":\"B-2 (L-CORE)\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.416Z\",\"verb\":\"ITEM\",\"data\":{\"key\":\"B-2\"},\"stream\":\"inbox:andysol01\",\"seq\":1,\"from\":\"agmenhub0\",\"from_handle\":\"hub\",\"from_kind\":\"hub\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.416Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8EZ3A0NSJDV74RT52XYM\",\"reply_to\":\"01M5VN8EY6P64XANJ47QNGW3FK\",\"to\":[],\"mentions\":[],\"items\":[\"B-2\"],\"lane\":\"L-CORE\",\"body\":\"B-2 granted to andy/sol until 2026-10-26T08:59:31.478Z\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.491Z\",\"verb\":\"GRANT\",\"data\":{\"item\":\"B-2\",\"lease_until\":\"2026-10-26T08:59:31.478Z\"},\"stream\":\"inbox:andysol01\",\"seq\":2,\"from\":\"agmenhub0\",\"from_handle\":\"hub\",\"from_kind\":\"hub\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.491Z\"}}",
    "{\"kind\":\"delivery\",\"id\":\"01M5VN8F3CF66ZSW679K182C2A\",\"state\":{\"state\":\"pending\",\"attempts\":1}}",
    "{\"kind\":\"delivery\",\"id\":\"01M5VN8F3CF66ZSW679K182C2A\",\"state\":{\"state\":\"recorded\",\"stream\":\"feed\",\"seq\":9}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8F3CF66ZSW679K182C2A\",\"reply_to\":null,\"to\":[],\"mentions\":[],\"items\":[],\"lane\":null,\"body\":\"\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.628696Z\",\"verb\":\"HELLO\",\"data\":{\"client\":\"agmen-cli/0.1.0\",\"agent\":null,\"model\":null,\"looking_at\":null},\"stream\":\"feed\",\"seq\":9,\"from\":\"andysol01\",\"from_handle\":\"andy/sol\",\"from_kind\":\"agent\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.66Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8F4D199GDJ23EPXCEWNG\",\"reply_to\":null,\"to\":[\"andysol01\"],\"mentions\":[],\"items\":[\"B-2\"],\"lane\":null,\"body\":\"look at it when you can\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.661487Z\",\"verb\":\"DM\",\"data\":{},\"stream\":\"inbox:andysol01\",\"seq\":3,\"from\":\"adamcoord01\",\"from_handle\":\"adam/coord\",\"from_kind\":\"coordinator\",\"mentioned\":[],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.704Z\"}}",
    "{\"kind\":\"message\",\"message\":{\"v\":1,\"id\":\"01M5VN8F7BBA4J20H3FBM6PACM\",\"reply_to\":null,\"to\":[],\"mentions\":[\"andysol01\"],\"items\":[],\"lane\":null,\"body\":\"heads up on the build\",\"valid_until\":null,\"sent_at\":\"2026-10-26T08:29:31.755195Z\",\"verb\":\"FYI\",\"data\":{},\"stream\":\"feed\",\"seq\":10,\"from\":\"adamcoord01\",\"from_handle\":\"adam/coord\",\"from_kind\":\"coordinator\",\"mentioned\":[\"andysol01\"],\"refs\":[],\"hub_at\":\"2026-10-26T08:29:31.804Z\"}}"
  ],
  "check_path": [
    {
      "paths": [
        "/work/ui/a.ts"
      ],
      "stdout": "[{\"path\":\"/work/ui/a.ts\",\"lane\":\"L-UI\",\"allowed\":false,\"reason\":\"lane L-UI held by adam/coord\"}]\n"
    },
    {
      "paths": [
        "/work/src/ok.ts"
      ],
      "stdout": "[{\"path\":\"/work/src/ok.ts\",\"lane\":\"L-CORE\",\"allowed\":true,\"reason\":\"lane L-CORE held by andy/sol\"}]\n"
    }
  ]
}
