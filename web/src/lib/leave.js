// Unsaved work on a page: the page registers a guard, and the hash router
// asks it before following a link or the back button elsewhere.

let guard = null;

/** `mayGo()` answers whether to leave (it may ask the person). Returns the
 *  function that lifts the guard. */
export function holdLeave(mayGo) {
  guard = mayGo;
  return () => {
    if (guard === mayGo) guard = null;
  };
}

/** Whether the page shown may be left now. */
export const mayLeave = () => !guard || guard();
