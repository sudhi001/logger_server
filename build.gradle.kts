import org.jetbrains.kotlin.gradle.tasks.KotlinCompile
import org.springframework.boot.gradle.tasks.bundling.BootBuildImage

plugins {
	val kotlinVersion = "2.0.0"
	id("org.springframework.boot") version "3.3.1"
	id("io.spring.dependency-management") version "1.1.5"
	kotlin("jvm") version kotlinVersion
	kotlin("plugin.spring") version kotlinVersion
	id("com.palantir.docker") version "0.36.0"
}

group = "in.sudhi.app"
version = "1.0.1-SNAPSHOT"

java {
	toolchain {
		languageVersion = JavaLanguageVersion.of(17)
	}
}

repositories {
	mavenCentral()
}

dependencies {
	implementation("org.springframework.boot:spring-boot-starter-web")
	implementation("org.springframework.boot:spring-boot-starter-data-jdbc")
	implementation("org.springframework.boot:spring-boot-starter-data-jpa")
	implementation( "org.jetbrains.kotlinx:kotlinx-coroutines-core:1.5.2")
			implementation( "org.jetbrains.kotlinx:kotlinx-coroutines-jdk8:1.5.2")
	implementation("org.jetbrains.kotlin:kotlin-reflect")
	implementation("com.fasterxml.jackson.module:jackson-module-kotlin")
	implementation("org.springframework.boot:spring-boot-starter-hateoas")
	implementation("com.h2database:h2")

	testImplementation("org.springframework.boot:spring-boot-starter-test")
}

kotlin {
	compilerOptions {
		freeCompilerArgs.addAll("-Xjsr305=strict")

	}
}

tasks.named<BootBuildImage>("bootBuildImage") {
	// For multi arch (Apple Silicon) support
	builder.set("paketobuildpacks/builder-jammy-buildpackless-tiny")
	buildpacks.set(listOf("paketobuildpacks/java"))
}
tasks.withType<KotlinCompile> {
	kotlinOptions {
		freeCompilerArgs = listOf("-Xjsr305=strict")
		jvmTarget = "17"
	}
}

tasks.withType<Test> {
	useJUnitPlatform()
}

docker {
	name = "${project.group}/${rootProject.name}:${project.version}"
	files(tasks.bootJar.get())
	buildArgs(mapOf("JAR_FILE" to tasks.bootJar.get().archiveFileName.get()))
}