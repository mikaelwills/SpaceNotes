use std::fmt::Display;
use std::panic::{catch_unwind, AssertUnwindSafe};

pub fn run_isolated<F, C>(context: C, body: F) -> bool
where
    F: FnOnce(),
    C: Display,
{
    let result = catch_unwind(AssertUnwindSafe(body));
    if result.is_err() {
        tracing::error!("Skipping item that panicked: {}", context);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naive_loop_aborts_batch_when_one_item_panics() {
        let mut processed = Vec::new();
        let items = vec![1, 2, 3, 4, 5];

        let result = catch_unwind(AssertUnwindSafe(|| {
            for item in &items {
                if *item == 3 {
                    panic!("item 3 blew up");
                }
                processed.push(*item);
            }
        }));

        assert!(result.is_err(), "expected the naive loop to unwind");
        assert_eq!(
            processed,
            vec![1, 2],
            "naive loop should abort mid-batch, not skip-and-continue"
        );
    }

    #[test]
    fn run_isolated_skips_panicking_item_and_continues_batch() {
        let mut processed = Vec::new();
        let items = vec![1, 2, 3, 4, 5];

        for item in &items {
            let ok = run_isolated(format!("item {}", item), || {
                if *item == 3 {
                    panic!("item 3 blew up");
                }
            });
            if ok {
                processed.push(*item);
            }
        }

        assert_eq!(
            processed,
            vec![1, 2, 4, 5],
            "run_isolated should skip the panicking item and process the rest"
        );
    }

    #[test]
    fn run_isolated_returns_true_when_body_succeeds() {
        assert!(run_isolated("clean item", || {}));
    }
}
