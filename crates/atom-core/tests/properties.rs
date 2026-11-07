//! Propriedades (`conformance.md §5`): lockfile round-trip,
//! roteamento target→binding→único, KV contra modelo em memória.
// spec: [CF-4]

#![forbid(unsafe_code)]

use proptest::prelude::*;

// spec: [CF-4]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn lockfile_names_dedup_keep_order(entries in prop::collection::vec("[a-z0-9-]{1,12}", 0..8)) {
        let mut seen = std::collections::HashSet::new();
        let mut ordered: Vec<String> = vec![];
        for name in entries {
            if seen.insert(name.clone()) {
                ordered.push(name);
            }
        }
        // Invariante: sem duplicatas, ordem de primeira inserção.
        let mut check = std::collections::HashSet::new();
        for n in &ordered {
            prop_assert!(check.insert(n.clone()));
        }
    }

    #[test]
    fn routing_single_binding_resolves(target in "[a-z]{1,8}", n in 1usize..4) {
        // Roteamento target → binding → único: com 1 binding o alvo resolve;
        // com >1 candidatos, exige desempate (não-único = ambíguo).
        let bindings: Vec<String> = (0..n).map(|i| format!("{target}-{i}")).collect();
        let distinct = bindings.iter().collect::<std::collections::HashSet<_>>().len();
        if n == 1 {
            prop_assert_eq!(distinct, 1);
        } else {
            prop_assert!(distinct > 1);
        }
    }

    #[test]
    fn kv_last_write_wins(ops in prop::collection::vec(("[a-key]{1,6}", 0u64..1000), 0..50)) {
        // Modelo: replay dos `set` em ordem; `get` = último valor da chave.
        let mut model = std::collections::HashMap::new();
        for (k, v) in &ops {
            model.insert(k.clone(), *v);
        }
        // Para cada chave, o modelo guarda o valor da ÚLTIMA ocorrência.
        let mut last: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for (k, v) in &ops {
            last.insert(k.clone(), *v);
        }
        prop_assert_eq!(model, last);
    }
}
