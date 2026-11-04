# Recorded runs

Both runs were recorded on an Android emulator with drengr 0.10.13, against a demo shop app.
Responses are shortened to the fields that matter.

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
   [1] lipstick (EditText, focused)
   [3] Clear (View, read-only)
   ```

   "Clear" moved from 4 to 3: element numbers are reassigned after every change.

4. `drengr_do(action="key", keycode="enter", format="text")`

   ```json
   {"network_calls": [{"method": "GET", "status": 200, "duration_ms": 212,
                       "url": "dummyjson.com/products/search?q=lipstick"}]}
   ```

   ```text
   [4] Red Lipstick · $12.99 · 4.4 (View)
   ```

5. `drengr_query(question="assert", conditions='[{"text": "Red Lipstick", "visible": true}]')`

   ```json
   {"pass": true, "results": [{"met": true, "reason": "Found 'Red Lipstick'"}]}
   ```

Report:

- **Result:** done
- **Steps:** 1. launched the shop → home list; 2. typed "lipstick" into search → field filled;
  3. pressed enter → GET search?q=lipstick 200, one result
- **Evidence:** assert passed for "Red Lipstick"; the search call returned 200 in 212 ms

## Run 2: a tap that did not do what was intended

1. `drengr_do(action="tap", element=4, format="text")` on the "Red Lipstick" result.

   ```json
   {"situation": {"activity_changed": false, "stuck": false,
                  "new_elements": ["Search products…", "Essence Mascara Lash Princess · $9.99 · 2.6"]}}
   ```

   No product page opened: the search was cleared and the full list came back.
   `activity_changed` is false in this single-activity app whether or not the screen changes, so
   the new elements are what show it. Next step: `drengr_look(format="image")` to see the
   screen before acting again.

2. With the search field focused and the keyboard open:
   `drengr_do(action="tap", element_text="Nonexistent Widget", scroll_to_find=true, max_scroll=3)`

   ```text
   isError: Element 'Nonexistent Widget' not found after 3 scroll attempts.
   [1] GT GT GT (EditText, focused)
   ```

   Each scroll typed "GT" into the field. After `drengr_do(action="back")`,
   `drengr_query(question="keyboard")` returned `{"visible": false}`, and the same call scrolled
   the list without typing anything.
