// View for the statechart of Admission.tla: one region for each call, one for the trace.
// The words are the shim's and the operator's, not the code's.

export const title = "Two BeforeTool or AfterTool calls, each blob or trace write may fail";

const CALLS = { k1: "Call 1", k2: "Call 2" };

const callState = (v, k) => {
  switch (v.pc[k]) {
    case "new":
      return "Waiting for the lock";
    case "locked":
      return "Holds the lock, deciding on a copy";
    case "early_commit":
    case "stepped":
      return "Decided, about to store the payload";
    case "writing":
      return "Storing the payload blobs, then writing the event";
    case "to_commit":
      return "Event written, state not yet advanced";
    case "committed":
      return "State advanced, still holds the lock";
    case "returned":
      return "Lock freed, reply on its way";
    case "failed":
      return "Store or write failed, lock freed, state unchanged";
    case "done":
      switch (v.reply[k]) {
        case "Err":
          return "Shim got an error";
        case "Deny":
          return "Shim got a Deny, nothing recorded";
        case "Refused":
          return "Report refused, nothing recorded";
        case "Ack":
          return "Report recorded";
        default:
          return "Shim got an Allow";
      }
    default:
      return v.pc[k];
  }
};

export const state = (v) => {
  const rows = Object.keys(v.trace).length;
  return {
    [CALLS.k1]: callState(v, "k1"),
    [CALLS.k2]: callState(v, "k2"),
    Trace: rows === 0 ? "No events" : `${rows} event${rows === 1 ? "" : "s"}`,
  };
};

export const step = (action) => {
  switch (action) {
    case "Lock":
      return "the call takes the lock";
    case "Step":
      return "the core decides on a copy of the state";
    case "StartWrites":
      return "the append begins";
    case "BlobOk":
      return "a payload blob is stored and flushed";
    case "BlobFails":
      return "storing a payload blob fails";
    case "AppendOk":
      return "the event is written to the trace";
    case "AppendFails":
      return "the write to the trace fails, leaving the stored blobs unused";
    case "AppendIdClash":
      return "the write clashes with the other call's event";
    case "NoRecord":
      return "there is nothing to write";
    case "Commit":
      return "the state advances";
    case "Unlock":
      return "the call returns and frees the lock";
    case "Reply":
      return "the shim gets its answer";
    default:
      return null;
  }
};
