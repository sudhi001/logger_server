package `in`.sudhi.server.logger

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PathVariable
import org.springframework.web.bind.annotation.PostMapping
import org.springframework.web.bind.annotation.RequestBody
import org.springframework.web.bind.annotation.RestController
import org.springframework.web.servlet.mvc.method.annotation.SseEmitter
import java.io.IOException
import java.util.concurrent.CopyOnWriteArrayList

@RestController
class LogsController(private val repository: LogsRepository) {
	private val emitters = CopyOnWriteArrayList<SseEmitter>()
	@GetMapping("/logs")
	fun findAll() = repository.findAll()

	@GetMapping("/logs/{name}")
	fun findByLastName(@PathVariable name:String)
			= repository.findByName(name)

	@PostMapping("/logs")
	fun addLog(@RequestBody log: LogEntity): LogEntity {
		val maxMessageLength = 50384 // Maximum length for the message
		val truncatedMessage = if (log.message.length > maxMessageLength) {
			log.message.substring(0, maxMessageLength)
		} else {
			log.message
		}
		val truncatedLog = log.copy(message = truncatedMessage)
		val savedLog = repository.save(truncatedLog)
		sendLogToClients(savedLog)
		return savedLog
	}
	@GetMapping("/logs/recent")
	fun findRecentLogs(): List<LogEntity>{
		return repository.findTop1000ByOrderByIdDesc()
	}

	@GetMapping("/logs/stream")
	fun streamLogs(): SseEmitter {
		val emitter = SseEmitter()
		emitters.add(emitter)
		emitter.onCompletion { emitters.remove(emitter) }
		emitter.onTimeout { emitters.remove(emitter) }
		return emitter
	}

	private fun sendLogToClients(log: LogEntity) {
		CoroutineScope(Dispatchers.IO).launch {
			val deadEmitters = mutableListOf<SseEmitter>()
			emitters.forEach { emitter ->
				try {
					emitter.send(SseEmitter.event().data(log))
				} catch (e: IOException) {
//					// Handle the exception as needed, e.g., log it
//					println("Failed to send log to client: ${e.message}")
					deadEmitters.add(emitter)
				}
			}
			emitters.removeAll(deadEmitters.toSet())
		}
	}
}