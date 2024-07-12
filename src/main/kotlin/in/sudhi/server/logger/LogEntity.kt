package `in`.sudhi.server.logger

import jakarta.persistence.Entity
import jakarta.persistence.GeneratedValue
import jakarta.persistence.GenerationType
import jakarta.persistence.Id
import jakarta.persistence.Column

@Entity
data class LogEntity(
	@Id @GeneratedValue(strategy = GenerationType.IDENTITY)
	val id: Long = 0,

	@Column(length = 50384)
	val message: String,

	val name: String
){
	constructor() : this(0, "", "")
}

