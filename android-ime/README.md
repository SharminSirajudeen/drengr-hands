# Drengr Keyboard

An Android input method that types any Unicode text sent from the host.
`adb shell input text` cannot type non-ASCII text, so `AdbTransport::type_text`
switches to this keyboard, sends the text, then switches back to the user's own keyboard.

```
adb shell am broadcast -a dev.drengr.ime.COMMIT --es b64 <base64 of the UTF-8 text>
```

The broadcast reports `result=1` only when the text was committed into a focused editor.
Only senders holding `INJECT_EVENTS` can deliver it, which in practice means the adb shell.

`drengr-ime.apk` is the prebuilt APK that the drengr binary embeds and installs on first use.
To rebuild it you need the signing key, which is kept outside the repo (see `build.gradle.kts`):

```
ANDROID_HOME=<sdk> ./gradlew assembleRelease
cp build/outputs/apk/release/drengr-ime-release.apk drengr-ime.apk
```
