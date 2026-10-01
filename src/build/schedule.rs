//! Run the targets of a dependency graph, several at once where the graph
//! allows.
//!
//! A target starts once every target it depends on has been built. Among the
//! targets ready at a moment the earliest in the graph's order goes first, so
//! with one job the targets are built exactly in that order, and with more the
//! order is the same wherever the graph forces one.
//!
//! A target whose dependency failed is not built: it is reported as skipped,
//! naming the dependency. Without keep-going, the first failure stops any
//! further target from starting; the ones already running finish.

use std::sync::{Condvar, Mutex};

use anyhow::Result;

/// Targets by position, each with the positions it depends on.
pub struct Graph {
    pub names: Vec<String>,
    pub deps: Vec<Vec<usize>>,
}

/// What became of one target.
pub enum Outcome {
    /// Built, or found nothing to do.
    Done,
    /// Its job returned this error.
    Failed(anyhow::Error),
    /// Not started: the named dependency did not get built.
    Skipped { dependency: String },
    /// Not started: an earlier failure stopped the run.
    Unstarted,
}

struct State {
    started: Vec<bool>,
    /// `Some(true)` built, `Some(false)` failed or skipped.
    done: Vec<Option<bool>>,
    outcomes: Vec<Option<Outcome>>,
    running: usize,
    stopped: bool,
}

/// Build every target of `graph` with `job`, on up to `jobs` threads.
pub fn run<F>(graph: &Graph, jobs: usize, keep_going: bool, job: F) -> Vec<Outcome>
where
    F: Fn(usize) -> Result<()> + Sync,
{
    let n = graph.names.len();
    let state = Mutex::new(State {
        started: vec![false; n],
        done: vec![None; n],
        outcomes: (0..n).map(|_| None).collect(),
        running: 0,
        stopped: false,
    });
    let changed = Condvar::new();

    let worker = || {
        let mut st = state.lock().unwrap();
        loop {
            // Settle what can be settled without running anything: a target
            // whose dependency failed is skipped, and after a stop nothing new
            // starts.
            let mut next = None;
            for i in 0..n {
                if st.started[i] {
                    continue;
                }
                if st.stopped {
                    st.started[i] = true;
                    st.done[i] = Some(false);
                    st.outcomes[i] = Some(Outcome::Unstarted);
                    continue;
                }
                let mut ready = true;
                let mut failed_dep = None;
                for &d in &graph.deps[i] {
                    match st.done[d] {
                        Some(true) => {}
                        Some(false) => failed_dep = Some(d),
                        None => ready = false,
                    }
                }
                if let Some(d) = failed_dep {
                    st.started[i] = true;
                    st.done[i] = Some(false);
                    st.outcomes[i] = Some(Outcome::Skipped { dependency: graph.names[d].clone() });
                    changed.notify_all();
                    continue;
                }
                if ready && next.is_none() {
                    next = Some(i);
                }
            }
            let Some(i) = next else {
                if st.running > 0 {
                    // Nothing ready now: wait for a running target to finish.
                    st = changed.wait(st).unwrap();
                    continue;
                }
                // Nothing running and nothing ready: everything is settled, or
                // what is left waits on one another in a cycle. Build the
                // earliest of those anyway rather than hang.
                match (0..n).find(|&i| !st.started[i]) {
                    Some(i) => {
                        st.started[i] = true;
                        st.running += 1;
                        drop(st);
                        let r = job(i);
                        st = state.lock().unwrap();
                        finish(&mut st, i, r, keep_going);
                        changed.notify_all();
                        continue;
                    }
                    None => return,
                }
            };
            st.started[i] = true;
            st.running += 1;
            drop(st);
            let r = job(i);
            st = state.lock().unwrap();
            finish(&mut st, i, r, keep_going);
            changed.notify_all();
        }
    };

    if jobs <= 1 || n <= 1 {
        worker();
    } else {
        std::thread::scope(|scope| {
            for _ in 0..jobs.min(n) {
                spawn_worker(scope, &worker);
            }
        });
    }

    let st = state.into_inner().unwrap();
    st.outcomes.into_iter().map(|o| o.unwrap_or(Outcome::Unstarted)).collect()
}

fn finish(st: &mut State, i: usize, r: Result<()>, keep_going: bool) {
    st.running -= 1;
    match r {
        Ok(()) => {
            st.done[i] = Some(true);
            st.outcomes[i] = Some(Outcome::Done);
        }
        Err(e) => {
            st.done[i] = Some(false);
            st.outcomes[i] = Some(Outcome::Failed(e));
            if !keep_going {
                st.stopped = true;
            }
        }
    }
}

/// A builder thread with a stack deep enough for the reasoner's recursion, as
/// the main thread has; a system that refuses the reservation gets a smaller
/// one.
fn spawn_worker<'scope, 'env, F>(scope: &'scope std::thread::Scope<'scope, 'env>, f: &'env F)
where
    F: Fn() + Sync,
{
    const GIB: usize = 1024 * 1024 * 1024;
    let want = std::env::var("OWLMAKE_STACK_GIB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|g| g.saturating_mul(GIB))
        .unwrap_or(GIB.saturating_mul(4));
    for size in [want, GIB.saturating_mul(2), GIB, GIB / 4] {
        if size == 0 || size > want {
            continue;
        }
        if std::thread::Builder::new().stack_size(size).spawn_scoped(scope, f).is_ok() {
            return;
        }
    }
    std::thread::Builder::new().spawn_scoped(scope, f).expect("spawning a builder thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn graph(edges: &[(usize, &[usize])]) -> Graph {
        Graph {
            names: (0..edges.len()).map(|i| format!("t{i}")).collect(),
            deps: edges.iter().map(|(_, d)| d.to_vec()).collect(),
        }
    }

    /// Every target starts after what it depends on, however many threads run.
    #[test]
    fn dependencies_finish_before_dependents_start() {
        let g = graph(&[(0, &[]), (1, &[0]), (2, &[0]), (3, &[1, 2]), (4, &[])]);
        for jobs in [1, 3] {
            let order = Mutex::new(Vec::new());
            let outcomes = run(&g, jobs, false, |i| {
                order.lock().unwrap().push(i);
                std::thread::sleep(std::time::Duration::from_millis(5));
                Ok(())
            });
            assert!(outcomes.iter().all(|o| matches!(o, Outcome::Done)));
            let order = order.into_inner().unwrap();
            let pos = |i: usize| order.iter().position(|&x| x == i).unwrap();
            assert!(pos(0) < pos(1) && pos(0) < pos(2) && pos(1) < pos(3) && pos(2) < pos(3));
            if jobs == 1 {
                assert_eq!(order, vec![0, 1, 2, 3, 4]);
            }
        }
    }

    /// A failed target's dependents are skipped and named; without keep-going
    /// nothing further starts.
    #[test]
    fn a_failure_skips_its_dependents() {
        let g = graph(&[(0, &[]), (1, &[0]), (2, &[]), (3, &[2])]);
        let ran = AtomicUsize::new(0);
        let outcomes = run(&g, 1, true, |i| {
            ran.fetch_add(1, Ordering::Relaxed);
            if i == 0 { anyhow::bail!("no") } else { Ok(()) }
        });
        assert!(matches!(outcomes[0], Outcome::Failed(_)));
        assert!(matches!(&outcomes[1], Outcome::Skipped { dependency } if dependency == "t0"));
        assert!(matches!(outcomes[2], Outcome::Done));
        assert!(matches!(outcomes[3], Outcome::Done));
        assert_eq!(ran.load(Ordering::Relaxed), 3);

        let outcomes = run(&g, 1, false, |i| if i == 0 { anyhow::bail!("no") } else { Ok(()) });
        assert!(matches!(outcomes[0], Outcome::Failed(_)));
        assert!(matches!(outcomes[2], Outcome::Unstarted));
    }
}
