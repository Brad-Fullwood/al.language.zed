//! The walk over recorded statements that a snapshot replay steps through.
//!
//! Pure data in, positions out: no I/O and no DAP types. The rules follow
//! Microsoft's adapter as observed in its traces (see the module docs of
//! [`super`]).
//!
//! A position is `(context, statement)`, both indexes into the contexts in
//! execution order. Contexts are visited in that order, statements inside a
//! context in recorded order. The call nesting comes from each context's
//! recorded frames: the caller of a context is the nearest earlier context
//! whose frames are the callee's frames without the first, and the call site
//! is the statement of the caller whose span is the callee's second frame.

use std::collections::HashMap;

use al_bc::snapshot::archive::{MethodContext, ObjectType};

/// A recorded statement: index of the context, index of the statement in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub ctx: usize,
    pub line: usize,
}

/// A source line of an object, the identity breakpoints and snappoints share.
/// `line` is 0-based like the recorded spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineKey {
    pub object_type: i8,
    pub number: i32,
    pub line: u16,
}

/// Where a move ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Stopped at a statement.
    Stopped(Landing),
    /// Nothing left to walk. The replay is over.
    Ended,
}

/// A stop and what led to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Landing {
    pub pos: Pos,
    /// The walk restarted at the first recorded statement.
    pub start_over: bool,
    /// The statement is where the context's exception was raised.
    pub exception: bool,
}

/// One user breakpoint line as the client sent it, reported back verified or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserBreakpoint {
    pub key: LineKey,
    /// The line has at least one recorded statement.
    pub verified: bool,
}

pub struct Engine {
    /// Statements per context.
    lens: Vec<usize>,
    /// Line key of every statement.
    keys: Vec<Vec<LineKey>>,
    /// Statement of each context where its exception was raised.
    exception_at: Vec<Option<usize>>,
    /// Call site of each context in its caller.
    parent: Vec<Option<Pos>>,
    /// Contexts called from a statement, in call order.
    callees: HashMap<Pos, Vec<usize>>,
    /// Every statement of a line, in walk order.
    occurrences: HashMap<LineKey, Vec<Pos>>,
    /// Lines of statements that carry variables or an exception, first seen first.
    snappoints: Vec<LineKey>,
    /// Snappoint lines the host reports in `al/updateSnappointsEvent`: lines with
    /// variables on a statement that is not the error statement.
    listed: Vec<LineKey>,
    /// User breakpoint lines per source the client named.
    user: HashMap<String, Vec<LineKey>>,
    /// Occurrences of each line already passed.
    hits: HashMap<LineKey, usize>,
    current: Option<Pos>,
}

impl Engine {
    pub fn new(contexts: &[MethodContext]) -> Self {
        let mut keys = Vec::with_capacity(contexts.len());
        let mut exception_at = Vec::with_capacity(contexts.len());
        let mut occurrences: HashMap<LineKey, Vec<Pos>> = HashMap::new();
        let mut snappoints = Vec::new();
        let mut listed = Vec::new();
        for (c, ctx) in contexts.iter().enumerate() {
            let exc = ctx.exception.as_ref().map(|e| {
                usize::try_from(e.line_number)
                    .ok()
                    .filter(|i| *i < ctx.line_contexts.len())
                    .unwrap_or(ctx.line_contexts.len().saturating_sub(1))
            });
            let exc = exc.filter(|_| !ctx.line_contexts.is_empty());
            exception_at.push(exc);
            let mut row = Vec::with_capacity(ctx.line_contexts.len());
            for (l, lc) in ctx.line_contexts.iter().enumerate() {
                let key = LineKey {
                    object_type: ctx.id.object_type.as_i8(),
                    number: ctx.id.number,
                    line: lc.statement_span.from.line,
                };
                occurrences
                    .entry(key)
                    .or_default()
                    .push(Pos { ctx: c, line: l });
                if (!lc.variables.is_empty() || exc == Some(l)) && !snappoints.contains(&key) {
                    snappoints.push(key);
                }
                if !lc.variables.is_empty() && exc != Some(l) && !listed.contains(&key) {
                    listed.push(key);
                }
                row.push(key);
            }
            keys.push(row);
        }
        let (parent, callees) = link_calls(contexts);
        Engine {
            lens: contexts.iter().map(|c| c.line_contexts.len()).collect(),
            keys,
            exception_at,
            parent,
            callees,
            occurrences,
            snappoints,
            listed,
            user: HashMap::new(),
            hits: HashMap::new(),
            current: None,
        }
    }

    pub fn snappoints(&self) -> &[LineKey] {
        &self.snappoints
    }

    pub fn listed_snappoints(&self) -> &[LineKey] {
        &self.listed
    }

    pub fn current(&self) -> Option<Pos> {
        self.current
    }

    /// The key of a statement.
    pub fn key_at(&self, pos: Pos) -> Option<LineKey> {
        self.keys.get(pos.ctx)?.get(pos.line).copied()
    }

    /// Replace the breakpoint lines of one source. `lines` are 1-based as the
    /// client sends them and equal the 1-based frame lines of the stops.
    /// A line that was already set keeps its hit count. A new line does not
    /// stop at statements the walk has passed.
    pub fn set_user_breakpoints(
        &mut self,
        source: &str,
        object_type: i8,
        number: i32,
        lines: &[i64],
    ) -> Vec<UserBreakpoint> {
        let mut keys = Vec::new();
        let mut out = Vec::new();
        for l in lines {
            let key = u16::try_from(l - 1).ok().map(|line| LineKey {
                object_type,
                number,
                line,
            });
            let Some(key) = key else {
                out.push(UserBreakpoint {
                    key: LineKey {
                        object_type,
                        number,
                        line: 0,
                    },
                    verified: false,
                });
                continue;
            };
            let verified = self.occurrences.contains_key(&key);
            if verified {
                keys.push(key);
                let passed = self.passed(key, self.current);
                self.hits.entry(key).or_insert(passed);
            }
            out.push(UserBreakpoint { key, verified });
        }
        self.user.insert(source.to_string(), keys);
        out
    }

    /// Occurrences of `key` at or before `pos`.
    fn passed(&self, key: LineKey, pos: Option<Pos>) -> usize {
        let Some(pos) = pos else { return 0 };
        self.occurrences
            .get(&key)
            .map_or(0, |o| o.partition_point(|p| *p <= pos))
    }

    fn active_keys(&self) -> Vec<LineKey> {
        let mut keys = self.snappoints.clone();
        for k in self.user.values().flatten() {
            if !keys.contains(k) {
                keys.push(*k);
            }
        }
        keys
    }

    /// First move after `configurationDone`: run to the first breakpoint, or
    /// start at the first recorded statement when there is none.
    pub fn start(&mut self) -> Step {
        if self.active_keys().is_empty() {
            self.start_over()
        } else {
            self.cont()
        }
    }

    /// Restart at the first recorded statement.
    pub fn start_over(&mut self) -> Step {
        if self.lens.first().is_none_or(|n| *n == 0) {
            return self.end();
        }
        self.land(Pos { ctx: 0, line: 0 }, true)
    }

    fn end(&mut self) -> Step {
        self.current = None;
        Step::Ended
    }

    fn land(&mut self, pos: Pos, start_over: bool) -> Step {
        for key in self.active_keys() {
            let passed = self.passed(key, Some(pos));
            let h = self.hits.entry(key).or_insert(0);
            *h = (*h).max(passed);
        }
        self.current = Some(pos);
        Step::Stopped(Landing {
            pos,
            start_over,
            exception: self.exception_at[pos.ctx] == Some(pos.line),
        })
    }

    /// Run to the next unused occurrence of any breakpoint.
    pub fn cont(&mut self) -> Step {
        let next = self
            .active_keys()
            .into_iter()
            .filter_map(|k| {
                let used = self.hits.get(&k).copied().unwrap_or(0);
                self.occurrences.get(&k)?.get(used).copied()
            })
            .min();
        match next {
            Some(pos) => self.land(pos, false),
            None => self.end(),
        }
    }

    /// The next statement of the same context, or the return to the caller.
    #[allow(clippy::should_implement_trait)] // DAP "next", not an iterator
    pub fn next(&mut self) -> Step {
        let Some(cur) = self.current else {
            return self.start();
        };
        if cur.line + 1 < self.lens[cur.ctx] {
            return self.land(
                Pos {
                    ctx: cur.ctx,
                    line: cur.line + 1,
                },
                false,
            );
        }
        self.step_out_of(cur.ctx)
    }

    /// Enter the first call recorded at the current statement. Without one
    /// the walk stays on the statement, as Microsoft's adapter does, except
    /// in the last recorded context: the host's GetNextFrameIndex returns
    /// index + 1 there, which is past the end, so the replay terminates.
    pub fn step_in(&mut self) -> Step {
        let Some(cur) = self.current else {
            return self.start();
        };
        match self.callees.get(&cur).and_then(|c| c.first()).copied() {
            Some(callee) if self.lens[callee] > 0 => self.land(
                Pos {
                    ctx: callee,
                    line: 0,
                },
                false,
            ),
            _ if cur.ctx + 1 >= self.lens.len() => self.end(),
            _ => self.land(cur, false),
        }
    }

    /// Leave the current context for the statement after its call site.
    pub fn step_out(&mut self) -> Step {
        let Some(cur) = self.current else {
            return self.start();
        };
        self.step_out_of(cur.ctx)
    }

    fn step_out_of(&mut self, ctx: usize) -> Step {
        match self.parent[ctx] {
            Some(site) => self.next_after(site),
            // An entry method that is not the first context restarts the walk.
            // The first context ends the replay.
            None if ctx == 0 => self.end(),
            None => self.start_over(),
        }
    }

    fn next_after(&mut self, site: Pos) -> Step {
        if site.line + 1 < self.lens[site.ctx] {
            self.land(
                Pos {
                    ctx: site.ctx,
                    line: site.line + 1,
                },
                false,
            )
        } else {
            self.step_out_of(site.ctx)
        }
    }

    /// Call sites from the entry method down to `pos`, as positions.
    pub fn stack(&self, pos: Pos) -> Vec<Pos> {
        let mut out = vec![pos];
        let mut ctx = pos.ctx;
        while let Some(site) = self.parent[ctx] {
            out.push(site);
            ctx = site.ctx;
            if out.len() > self.lens.len() {
                break;
            }
        }
        out.reverse();
        out
    }
}

/// Find the caller and call site of every context.
fn link_calls(contexts: &[MethodContext]) -> (Vec<Option<Pos>>, HashMap<Pos, Vec<usize>>) {
    // The latest context so far for each chain of (method, object) from the
    // context outward, keyed by the chain itself.
    let mut latest: HashMap<Vec<(String, ObjectType, i32)>, usize> = HashMap::new();
    let mut parent = vec![None; contexts.len()];
    let mut callees: HashMap<Pos, Vec<usize>> = HashMap::new();
    for (c, ctx) in contexts.iter().enumerate() {
        let chain: Vec<(String, ObjectType, i32)> = ctx
            .frames
            .iter()
            .map(|f| (f.method_name.clone(), f.id.object_type, f.id.number))
            .collect();
        if chain.len() > 1 {
            if let Some(&caller) = latest.get(&chain[1..]) {
                if let Some(site) = call_site(contexts, caller, c) {
                    parent[c] = Some(site);
                    callees.entry(site).or_default().push(c);
                }
            }
        }
        latest.insert(chain, c);
    }
    (parent, callees)
}

/// The statement of `caller` that made the call recorded in `callee`.
fn call_site(contexts: &[MethodContext], caller: usize, callee: usize) -> Option<Pos> {
    let callee_ctx = &contexts[callee];
    let span = callee_ctx.frames.get(1)?.statement_span;
    let lines = &contexts[caller].line_contexts;
    let start = callee_ctx.execution_span.start_time;
    let same: Vec<usize> = (0..lines.len())
        .filter(|i| {
            let s = lines[*i].statement_span;
            s.from.line == span.from.line && s.from.column == span.from.column
        })
        .collect();
    let same: Vec<usize> = if same.is_empty() {
        (0..lines.len())
            .filter(|i| lines[*i].statement_span.from.line == span.from.line)
            .collect()
    } else {
        same
    };
    // A statement that ran several times: the last run that began before the call.
    let line = same
        .iter()
        .copied()
        .rfind(|i| lines[*i].timestamp <= start)
        .or_else(|| same.first().copied())?;
    Some(Pos { ctx: caller, line })
}
