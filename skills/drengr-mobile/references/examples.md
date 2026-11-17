# Recorded runs

All three runs were recorded on 5 Nov 2026 over MCP with drengr 0.11.1, on an Android emulator,
against a demo shop app. Responses are shortened to the fields that matter.

## Run 1: search the shop and prove the result

1. `drengr_query(question="setup")`

   ```json
   {"ok": true, "device_id": "emulator-5554", "platform": "android",
    "apps": [{"display_name": "Demoshop", "package": "dev.drengr.demoshop.android"}]}
   ```

2. `drengr_do(action="launch", package="dev.drengr.demoshop.android", format="text")`

   ```text
   [1] Search products… (EditText)
   [4] Clear (View, read-only)
   [5] Essence Mascara Lash Princess · $9.99 · 2.6 (View)
   ```

3. `drengr_do(action="type", text="lipstick", element=1, format="text")`

   ```text
   new_elements: ["lipstick"]
   [22] lipstick (EditText, focused)
   [4] Clear (View, read-only)
   ```

   The field's label changed, so it got a new number (1 became 22). "Clear" did not change and
   kept 4.

4. `drengr_do(action="key", keycode="enter", format="text")`

   ```json
   {"network_calls": [{"method": "GET", "status": 200, "duration_ms": 208, "source": "logcat",
                       "url": "dummyjson.com/products/search?q=lipstick"}]}
   ```

   ```text
   [23] Red Lipstick · $12.99 · 4.4 (View)
   ```

5. `drengr_query(question="assert", conditions='[{"text": "Red Lipstick", "visible": true}]')`

   ```json
   {"pass": true, "results": [{"met": true, "reason": "Found 'Red Lipstick'"}]}
   ```

Report:

- **Result:** done
- **Steps:** 1. launched the shop → home list; 2. typed "lipstick" into search → field filled;
  3. pressed enter → GET search?q=lipstick 200, one result
- **Evidence:** assert passed for "Red Lipstick"; the search call returned 200 in 208 ms

## Run 2: a number that no longer exists

1. `drengr_do(action="tap", element=23, format="text")` opens the product page:
   `new_elements` includes the description and "Add to cart".
2. `drengr_do(action="back", format="text")` returns to the list. The search field reads
   "Search products…" again, so it is `[1]` again; `[22]` (the "lipstick" label) is gone.
3. `drengr_do(action="tap", element=22, format="text")`

   ```text
   isError: Element #22 not found. Use scroll_to_find=true to search off-screen.
   ```

   Act on numbers from the latest response; when one is missing, look again rather than retry.

## Run 3: scrolling with the keyboard open

1. `drengr_do(action="tap", element=1)` focuses the search field;
   `drengr_query(question="keyboard")` returns `{"visible": true}`.
2. `drengr_do(action="tap", element_text="Nonexistent Widget", scroll_to_find=true, max_scroll=3)`

   ```text
   isError: Element 'Nonexistent Widget' not found after 3 scroll attempts.
   ```

3. `drengr_look(format="text")`

   ```text
   [29] GT GT GT (EditText, focused)
   ```

   Each scroll typed "GT" into the field (issue #6). After `drengr_do(action="back")`,
   `drengr_query(question="keyboard")` returned `{"visible": false}`, and the same call left the
   field unchanged.
