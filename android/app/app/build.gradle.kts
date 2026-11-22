plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "app.phantom.host"
    compileSdk = 35

    defaultConfig {
        applicationId = "app.phantom.host"
        minSdk = 26
        targetSdk = 34
        versionCode = System.getenv("BUILD_NUMBER")?.toIntOrNull() ?: 1
        versionName = "1.3.2"

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    // Firma della release: keystore stabile passato dalla CI (secret) o da variabili d'ambiente locali.
    // Senza keystore la release si firma con la chiave di debug: resta installabile e, soprattutto,
    // NON è `debuggable`, quindi ART compila il codice al massimo (la build debug è molto più lenta
    // nel percorso caldo encoder → rete in modalità Standard).
    val releaseKeystore = System.getenv("PHANTOM_KEYSTORE_PATH")?.let { file(it) }?.takeIf { it.isFile }
    signingConfigs {
        if (releaseKeystore != null) {
            create("phantomRelease") {
                storeFile = releaseKeystore
                storePassword = System.getenv("PHANTOM_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("PHANTOM_KEY_ALIAS")
                keyPassword = System.getenv("PHANTOM_KEY_PASSWORD") ?: System.getenv("PHANTOM_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            isDebuggable = false
            signingConfig = if (releaseKeystore != null) {
                signingConfigs.getByName("phantomRelease")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }

    packaging {
        jniLibs {
            // Estrae libphantom_ffi.so e libjnidispatch.so in nativeLibraryDir così anche `app_process` le carica.
            useLegacyPackaging = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    lint {
        // Il lint completo resta disponibile con `gradle lint`; qui non deve bloccare la build della release in CI.
        checkReleaseBuilds = false
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.15.0@aar")
    implementation("com.google.zxing:core:3.5.3")
    // Modalità Shizuku (controllo completo senza root). Versione bloccata: `newProcess` sparisce con l'API 14.
    implementation("dev.rikka.shizuku:api:13.1.5")
    implementation("dev.rikka.shizuku:provider:13.1.5")
    testImplementation("junit:junit:4.13.2")
}
