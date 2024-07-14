# Use a base image with Java runtime
FROM --platform=linux/amd64 openjdk:17-jdk-slim

# Set the working directory
WORKDIR /app

# Add the jar file
ARG JAR_FILE=build/libs/*.jar
COPY ${JAR_FILE} app.jar

# Expose the port on which the app will run
EXPOSE 8080

# Run the jar file
ENTRYPOINT ["java", "-jar", "app.jar"]
