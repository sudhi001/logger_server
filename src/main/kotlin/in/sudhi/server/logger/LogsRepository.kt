package `in`.sudhi.server.logger

import org.springframework.data.repository.CrudRepository

interface LogsRepository : CrudRepository<LogEntity, Long> {

	fun findByName(name: String): Iterable<LogEntity>
	fun findTop1000ByOrderByIdDesc(): List<LogEntity>
}
