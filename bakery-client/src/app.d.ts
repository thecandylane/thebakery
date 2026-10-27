declare global {
  namespace App {
    interface PageState {
      /** Settings is open as a popup over the current page (a shallow route):
       *  the section shown. The page underneath stays mounted. */
      settings?: string;
    }
  }
}

export {};
