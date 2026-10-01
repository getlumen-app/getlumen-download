plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "io.getlumen.app"
    compileSdk {
        version = release(36)
    }

    defaultConfig {
        applicationId = "io.getlumen.app"
        minSdk = 23
        targetSdk = 36
        versionCode = 2
        versionName = "1.0.1"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

        ndk {
            // Sideloaded via Telegram (50MB Bot API upload cap) — ship arm64
            // only; every supported modern device uses it.
            abiFilters += setOf("arm64-v8a")
        }
    }

    signingConfigs {
        create("release") {
            val storeFilePath = System.getenv("LUMEN_KEYSTORE_FILE")
            if (storeFilePath != null) {
                storeFile = file(storeFilePath)
                storePassword = System.getenv("LUMEN_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("LUMEN_KEY_ALIAS") ?: "lumen-android"
                keyPassword = System.getenv("LUMEN_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Release builds are signed with the dedicated keystore when the
            // LUMEN_KEYSTORE_* env vars are set; otherwise Gradle falls back
            // to the local debug keystore so smoke builds still install.
            val hasReleaseKey = System.getenv("LUMEN_KEYSTORE_FILE") != null
            if (hasReleaseKey) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    packagingOptions {
        // extractNativeLibs=true only governs install-time extraction; keeping
        // compression on shrinks the APK below the Telegram upload limit while
        // libs still land in nativeLibraryDir for exec().
        jniLibs.useLegacyPackaging = false
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }
    kotlinOptions {
        jvmTarget = "11"
    }
}

dependencies {
    implementation(fileTree(mapOf("dir" to "libs", "include" to listOf("*.aar"))))
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.appcompat)
    implementation(libs.material)
    testImplementation(libs.junit)
    testImplementation(libs.orgjson)
}
