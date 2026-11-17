plugins {
    id("com.android.application") version "9.5.0"
}

android {
    namespace = "dev.drengr.ime"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.drengr.ime"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "1"
    }

    // One key for every release, read from ~/.gradle/gradle.properties, never
    // the repo: `adb install -r` refuses an APK signed by a different key.
    signingConfigs {
        create("release") {
            storeFile = file(providers.gradleProperty("drengrIme.storeFile").get())
            storePassword = providers.gradleProperty("drengrIme.password").get()
            keyAlias = "drengr-ime"
            keyPassword = providers.gradleProperty("drengrIme.password").get()
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
            signingConfig = signingConfigs.getByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
