// Host API prelude for workflow scripts.
//
// This expression evaluates to a function that Rust calls once with `natives`.
// Public APIs (agent, parallel, pipeline, phase, log, budget) live on globalThis;
// pending state and native references remain in the closure and cannot be forged.
// It returns [boot, resolve]: boot compiles and starts the script, and resolve
// settles the matching Promise. Rust retains both only through Persistent handles.
"use strict";
((natives) => {
  const issue = natives.issue;
  const nativeLog = natives.log;
  const nativePhase = natives.phase;
  const done = natives.done;
  const fail = natives.fail;
  const budgetTotal = natives.budgetTotal;
  const budgetSpent = natives.budgetSpent;
  const maxItems = natives.maxItems;

  // Block nondeterministic APIs before user code: replay requires the same
  // script to issue the same prompt-and-options sequence. Pass timestamps and seeds through args.
  const RealDate = Date;
  const dateError = () => {
    throw new Error(
      "Date.now()/无参 new Date() 在工作流脚本里不可用（会破坏恢复重放）；把时间戳经 args 传入"
    );
  };
  RealDate.now = dateError;
  globalThis.Date = new Proxy(RealDate, {
    construct(target, argumentsList, newTarget) {
      if (argumentsList.length === 0) dateError();
      return Reflect.construct(target, argumentsList, newTarget);
    },
    apply() {
      dateError();
    }
  });
  Math.random = () => {
    throw new Error(
      "Math.random() 在工作流脚本里不可用（会破坏恢复重放）；用下标或 args 里的种子构造差异"
    );
  };

  const pending = new Map();

  globalThis.agent = (prompt, opts) => {
    const id = issue(prompt, opts === undefined ? null : opts);
    return new Promise((resolve) => {
      pending.set(id, resolve);
    });
  };

  globalThis.parallel = (thunks) => {
    if (!Array.isArray(thunks)) {
      throw new TypeError("parallel(thunks) 需要一个函数数组");
    }
    if (thunks.length > maxItems) {
      throw new RangeError(
        "parallel 一次最多接受 " + maxItems + " 个 thunk，实际 " + thunks.length
      );
    }
    for (const thunk of thunks) {
      if (typeof thunk !== "function") {
        throw new TypeError("parallel 的每个元素都必须是函数（() => agent(...) 形式的 thunk）");
      }
    }
    // Barrier semantics: await every thunk; failed thunks become null and this call never rejects.
    return Promise.all(
      thunks.map((thunk) => (async () => thunk())().catch(() => null))
    );
  };

  globalThis.pipeline = (items, ...stages) => {
    if (!Array.isArray(items)) {
      throw new TypeError("pipeline(items, ...stages) 需要一个 items 数组");
    }
    if (items.length > maxItems) {
      throw new RangeError(
        "pipeline 一次最多接受 " + maxItems + " 个 item，实际 " + items.length
      );
    }
    if (stages.length === 0) {
      throw new TypeError("pipeline 至少需要一个 stage 函数");
    }
    for (const stage of stages) {
      if (typeof stage !== "function") {
        throw new TypeError("pipeline 的每个 stage 都必须是函数");
      }
    }
    // Each item passes independently through every stage without stage barriers.
    // A failed stage yields null for that item and skips its remaining stages.
    return Promise.all(
      items.map((item, index) =>
        (async () => {
          let value = item;
          for (const stage of stages) {
            value = await stage(value, item, index);
          }
          return value;
        })().catch(() => null)
      )
    );
  };

  globalThis.phase = (title) => {
    nativePhase(String(title));
  };

  globalThis.log = (message) => {
    nativeLog(typeof message === "string" ? message : String(message));
  };

  globalThis.budget = Object.freeze({
    get total() {
      const total = budgetTotal();
      return total === undefined ? null : total;
    },
    spent() {
      return budgetSpent();
    },
    remaining() {
      const total = budgetTotal();
      if (total === undefined || total === null) return Infinity;
      const left = total - budgetSpent();
      return left > 0 ? left : 0;
    }
  });

  const AsyncFunction = (async () => {}).constructor;

  const boot = (body) => {
    // AsyncFunction bodies allow top-level await and return; Rust receives syntax errors here as script failures.
    const main = new AsyncFunction(body);
    Promise.resolve(main.call(undefined)).then(
      (value) => {
        done(value);
      },
      (error) => {
        if (error instanceof Error) {
          const stack =
            typeof error.stack === "string" && error.stack !== ""
              ? "\n" + error.stack
              : "";
          fail(error.name + ": " + error.message + stack);
        } else {
          let text;
          try {
            text = String(error);
          } catch (_ignored) {
            text = "（不可字符串化的异常值）";
          }
          fail(text);
        }
      }
    );
  };

  const resolve = (id, value) => {
    const settle = pending.get(id);
    pending.delete(id);
    if (settle !== undefined) {
      settle(value);
    }
  };

  return [boot, resolve];
})
